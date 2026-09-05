package web

// The optimization loop: an optimizer model proposes variants of a subject,
// each variant (and the unchanged baseline) runs the operator's test on the
// target model, and the optimizer judges the outputs against the operator's
// goal — "make this handle the edge cases better", or "make this opus prompt
// hold up on sonnet" (target the smaller model, judge against the same bar).
// Nothing is applied automatically: a winning variant goes through the
// subject's ordinary validated edit path when the human clicks Apply, so an
// experiment can be wrong at zero cost beyond its own model calls.
//
// The pipeline (generate → run concurrently → judge) is generic; everything
// subject-specific lives behind experimentSubject, so adding a kind —
// workflow:<name> is the designed next one, where a variant is a graph and a
// run is a real workflow run — means one new implementation of that
// interface, not a second pipeline. An experiment is a row
// (store.Experiment) driven by one background goroutine, progress written to
// the row, the page polling; a daemon restart abandons a running experiment
// (readers report it as such) and re-running is one click.

import (
	"context"
	"encoding/json"
	"fmt"
	"math/rand"
	"net/http"
	"os"
	"sort"
	"strings"
	"sync"
	"time"

	"forge/internal/core/directives"
	"forge/internal/core/store"
)

const (
	experimentMaxVariants  = 12
	experimentCallTimeout  = 4 * time.Minute
	experimentTotalTimeout = 25 * time.Minute
	experimentRunners      = 3
	// experimentStaleAfter is how long a running row may sit without updates
	// before readers report it abandoned (a daemon restart orphans the worker).
	experimentStaleAfter = 10 * time.Minute
	// judgeOutputCap bounds each candidate's output inside the judge prompt.
	judgeOutputCap = 6 * 1024
)

// experimentSubject is everything kind-specific: what the baseline content
// is, what rules the optimizer must respect when varying it, whether a
// candidate is structurally valid, and how one candidate executes the test.
type experimentSubject interface {
	// generationRules appends kind-specific constraints to the optimizer's
	// instructions (file shape, placeholders to preserve, what exists to
	// reference).
	generationRules(sb *strings.Builder)
	// validate reports why a candidate cannot run ("" = it can) — the same
	// bar the kind's save path applies.
	validate(content string) string
	// run executes the test with the candidate's content in place and
	// returns the output the judge will score.
	run(ctx context.Context, content string, baseline bool) (string, error)
}

// experimentTest is the persona/routine test spec (one model completion over
// the composed prompt). A future workflow kind defines its own shape.
type experimentTest struct {
	Mode      string `json:"mode,omitempty"`
	Task      string `json:"task,omitempty"`
	Objective string `json:"objective,omitempty"`
	Repo      string `json:"repo,omitempty"`
}

// experimentCandidate is one entry of the stored results: the baseline or a
// proposed variant, its run, and the judge's read.
type experimentCandidate struct {
	Title          string  `json:"title"`
	Rationale      string  `json:"rationale,omitempty"`
	Content        string  `json:"content"`
	Output         string  `json:"output,omitempty"`
	Score          float64 `json:"score"`
	JudgeRationale string  `json:"judge_rationale,omitempty"`
	Baseline       bool    `json:"baseline,omitempty"`
	Error          string  `json:"error,omitempty"`
}

type experimentResults struct {
	Summary    string                `json:"summary,omitempty"`
	Best       string                `json:"best,omitempty"` // title of the judge's pick
	Candidates []experimentCandidate `json:"candidates"`
}

// ---- subjects ------------------------------------------------------------

// personaSubject varies a persona's whole file; a run composes the variant
// in memory (directives.WithVariant) and makes one completion on the target.
type personaSubject struct {
	s    *Server
	name string
	pe   *store.Experiment
	test experimentTest
}

func (p personaSubject) generationRules(sb *strings.Builder) {
	var names []string
	if lib := p.s.promptLibrary(); lib != nil {
		for _, f := range lib.Fragments() {
			if !f.Persona {
				names = append(names, f.Name)
			}
		}
	}
	fmt.Fprintf(sb, `- This is a persona file: preserve the frontmatter shape (--- fenced, model: <alias>), keep "## mode: <name>" section semantics, and only include fragments that exist: %s. {{> name}} includes and {{param}} placeholders are literal syntax — keep them intact unless removing an include is the point of the variant.
`, strings.Join(names, ", "))
}

func (p personaSubject) validate(content string) string {
	lib := p.s.promptLibrary()
	if lib == nil {
		return "no prompts library"
	}
	if _, err := lib.WithVariant(p.name, content); err != nil {
		return "does not compose: " + err.Error()
	}
	return ""
}

func (p personaSubject) run(ctx context.Context, content string, baseline bool) (string, error) {
	lib := p.s.promptLibrary()
	if lib == nil {
		return "", fmt.Errorf("no prompts library")
	}
	if !baseline {
		var err error
		if lib, err = lib.WithVariant(p.name, content); err != nil {
			return "", err
		}
	}
	rt := store.Routine{Name: "(experiment)", Mode: p.test.Mode, Prompt: p.test.Task, Persona: p.name}
	if rt.Mode == "" {
		rt.Mode = "run"
	}
	preview, err := p.s.renderPreviewLib(ctx, lib, rt, p.test.Objective, p.test.Repo)
	if err != nil {
		return "", err
	}
	return p.s.experimentModelCall(ctx, preview.Prompt, p.pe.TargetModel)
}

// directiveSubject varies a whole directive file: frontmatter and task text.
// A run composes the variant in a cloned library and previews a synthetic
// trigger routine through the real assembly path.
type directiveSubject struct {
	s    *Server
	name string
	pe   *store.Experiment
	test experimentTest
}

func (d directiveSubject) generationRules(sb *strings.Builder) {
	var names []string
	if lib := d.s.promptLibrary(); lib != nil {
		for _, f := range lib.Fragments() {
			if !f.Persona && !f.Directive {
				names = append(names, f.Name)
			}
		}
	}
	fmt.Fprintf(sb, `- This is a directive file: preserve the frontmatter shape (--- fenced; mode: is required, persona:/model:/effort: optional), keep {{objective}} and {{repo}} placeholders where present, and only include fragments that exist: %s. {{> name}} includes are literal syntax.
`, strings.Join(names, ", "))
}

func (d directiveSubject) validate(content string) string {
	lib := d.s.promptLibrary()
	if lib == nil {
		return "no prompts library"
	}
	if _, err := lib.WithVariant(d.name, content); err != nil {
		return "does not compose: " + err.Error()
	}
	return ""
}

func (d directiveSubject) run(ctx context.Context, content string, baseline bool) (string, error) {
	lib := d.s.promptLibrary()
	if lib == nil {
		return "", fmt.Errorf("no prompts library")
	}
	if !baseline {
		var err error
		if lib, err = lib.WithVariant(d.name, content); err != nil {
			return "", err
		}
	}
	rt := store.Routine{Name: "(experiment)", Target: "directive:" + d.name}
	preview, err := d.s.renderPreviewLib(ctx, lib, rt, d.test.Objective, d.test.Repo)
	if err != nil {
		return "", err
	}
	return d.s.experimentModelCall(ctx, preview.Prompt, d.pe.TargetModel)
}

// experimentSubjectFor resolves a subject string; workflow:<name> lands here
// as a third case when workflow experiments arrive.
func (s *Server) experimentSubjectFor(ctx context.Context, pe *store.Experiment) (experimentSubject, string, error) {
	kind, name, ok := strings.Cut(pe.Subject, ":")
	if !ok {
		return nil, "", badRequest("subject must be <kind>:<name> (persona or routine)")
	}
	var test experimentTest
	if len(pe.Test) > 0 {
		if err := json.Unmarshal(pe.Test, &test); err != nil {
			return nil, "", badRequest("test spec: %v", err)
		}
	}
	switch kind {
	case "persona":
		lib := s.promptLibrary()
		if lib == nil {
			return nil, "", badRequest("this process has no prompts library")
		}
		f := lib.Persona(name)
		if f == nil {
			return nil, "", badRequest("persona %q is not in the library", name)
		}
		raw, err := os.ReadFile(f.Path)
		if err != nil {
			return nil, "", err
		}
		return personaSubject{s: s, name: name, pe: pe, test: test}, string(raw), nil
	case "directive":
		lib := s.promptLibrary()
		if lib == nil {
			return nil, "", badRequest("this process has no prompts library")
		}
		f := lib.Directive(name)
		if f == nil {
			return nil, "", badRequest("directive %q is not in the library", name)
		}
		raw, err := os.ReadFile(f.Path)
		if err != nil {
			return nil, "", err
		}
		return directiveSubject{s: s, name: name, pe: pe, test: test}, string(raw), nil
	case "routine":
		rt, err := s.store.GetRoutine(ctx, name)
		if err != nil {
			return nil, "", err
		}
		return nil, "", badRequest("routine %s is a trigger — optimize its content as its target (%s)", name, rt.Target)
	}
	return nil, "", badRequest("unknown experiment subject kind %q", kind)
}

// experimentModelCall is one bounded completion.
func (s *Server) experimentModelCall(ctx context.Context, prompt, alias string) (string, error) {
	cctx, cancel := context.WithTimeout(ctx, experimentCallTimeout)
	defer cancel()
	return s.modelCall(cctx, "", prompt, alias)
}

// ---- HTTP ----------------------------------------------------------------

// createExperiment is POST /api/v1/experiments.
func (s *Server) createExperiment(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	if s.modelCall == nil {
		return 0, nil, badRequest("experiments need the daemon's model access, which this process does not have")
	}
	var req struct {
		Subject        string          `json:"subject"`
		Goal           string          `json:"goal"`
		TargetModel    string          `json:"target_model"`
		OptimizerModel string          `json:"optimizer_model"`
		Test           json.RawMessage `json:"test"`
		Variants       int             `json:"variants"`
		// Live experiments: arms assigned to real work at materialization.
		Live    bool   `json:"live"`
		From    string `json:"from"`     // offline experiment id to trial arms from
		MaxArms int    `json:"max_arms"` // including control
		MinRuns int    `json:"min_runs"` // per-arm decision window
		// Arms are operator-provided variant bodies (whole file content);
		// they skip generation, like the forge_experiment tool's variants.
		Arms []struct {
			Title   string `json:"title"`
			Content string `json:"content"`
		} `json:"arms"`
	}
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	req.Goal = strings.TrimSpace(req.Goal)
	if req.Goal == "" {
		return 0, nil, badRequest("state the goal — what should this do better?")
	}
	if req.Variants <= 0 {
		req.Variants = 8
	}
	if req.Variants > experimentMaxVariants {
		req.Variants = experimentMaxVariants
	}
	for _, alias := range []string{req.TargetModel, req.OptimizerModel} {
		if alias == "" {
			return 0, nil, badRequest("target_model and optimizer_model are required")
		}
		if _, ok := s.resolveModel(alias); !ok {
			return 0, nil, badRequest("unknown model alias %q", alias)
		}
	}
	pe := store.Experiment{
		Subject: req.Subject, Goal: req.Goal, TargetModel: req.TargetModel, OptimizerModel: req.OptimizerModel,
		Test: req.Test, VariantCount: req.Variants, Progress: "starting",
	}
	subject, baseline, err := s.experimentSubjectFor(ctx, &pe)
	if err != nil {
		return 0, nil, err
	}
	pe.Baseline = baseline
	if req.Live {
		minRuns, maxArms, _, _ := s.experimentsLimits()
		if req.MinRuns > 0 {
			pe.MinRuns = req.MinRuns
		} else {
			pe.MinRuns = minRuns
		}
		if req.MaxArms > 1 && req.MaxArms < maxArms {
			maxArms = req.MaxArms
		}
		pe.Kind = store.ExperimentKindLive
		pe.Progress = "preparing arms"
		if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.InsertExperiment(ctx, &pe) }); err != nil {
			return 0, nil, err
		}
		var provided []experimentCandidate
		for i, a := range req.Arms {
			title := a.Title
			if title == "" {
				title = fmt.Sprintf("variant %d", i+1)
			}
			provided = append(provided, experimentCandidate{Title: title, Content: a.Content})
		}
		go s.runLiveSetup(pe, subject, maxArms, req.From, provided)
		s.log.InfoContext(ctx, "live experiment started", "id", pe.ID, "subject", pe.Subject, "max_arms", maxArms, "min_runs", pe.MinRuns)
		return http.StatusCreated, map[string]string{"id": pe.ID}, nil
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.InsertExperiment(ctx, &pe) }); err != nil {
		return 0, nil, err
	}
	go s.runExperiment(pe, subject)
	s.log.InfoContext(ctx, "experiment started", "id", pe.ID, "subject", pe.Subject, "target", pe.TargetModel, "optimizer", pe.OptimizerModel, "variants", pe.VariantCount)
	return http.StatusCreated, map[string]string{"id": pe.ID}, nil
}

// listExperiments is GET /api/v1/experiments?subject=… (or ?id=… for one).
// Running rows gone quiet past the stale window read as failed — the daemon
// restarted under the worker.
func (s *Server) listExperiments(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if id := r.URL.Query().Get("id"); id != "" {
		pe, err := s.store.GetExperiment(ctx, id)
		if err != nil {
			return 0, nil, err
		}
		s.markStaleExperiment(pe)
		return http.StatusOK, s.experimentView(ctx, *pe), nil
	}
	subject := r.URL.Query().Get("subject")
	if subject == "" {
		return 0, nil, badRequest("subject or id is required")
	}
	list, err := s.store.Experiments(ctx, subject, 0)
	if err != nil {
		return 0, nil, err
	}
	if list == nil {
		list = []store.Experiment{}
	}
	out := make([]experimentView, 0, len(list))
	for i := range list {
		s.markStaleExperiment(&list[i])
		out = append(out, s.experimentView(ctx, list[i]))
	}
	return http.StatusOK, out, nil
}

// experimentView decorates a live row with its per-arm running tallies.
type experimentView struct {
	store.Experiment
	Tallies []armTally `json:"tallies,omitempty"`
}

func (s *Server) experimentView(ctx context.Context, pe store.Experiment) experimentView {
	v := experimentView{Experiment: pe}
	if pe.Kind == store.ExperimentKindLive && pe.Status == store.ExperimentLive {
		if t, err := s.liveExperimentTallies(ctx, pe.ID, pe.MinRuns); err == nil {
			v.Tallies = t
		}
	}
	return v
}

func (s *Server) markStaleExperiment(pe *store.Experiment) {
	if pe.Status == store.ExperimentRunning && s.now().Sub(pe.UpdatedAt) > experimentStaleAfter {
		pe.Status = store.ExperimentFailed
		pe.Error = "abandoned — no progress for " + experimentStaleAfter.String() + " (daemon restarted?)"
	}
}

// ---- the worker ----------------------------------------------------------

// runExperiment is the background pipeline: generation → runs → judgment.
func (s *Server) runExperiment(pe store.Experiment, subject experimentSubject) {
	ctx, cancel := context.WithTimeout(context.Background(), experimentTotalTimeout)
	defer cancel()
	fail := func(err error) {
		s.log.WarnContext(ctx, "experiment failed", "id", pe.ID, "error", err)
		werr := s.store.Write(context.WithoutCancel(ctx), func(tx *store.Tx) error {
			return tx.FinishExperiment(ctx, pe.ID, store.ExperimentFailed, nil, err.Error())
		})
		if werr != nil {
			s.log.ErrorContext(ctx, "record experiment failure", "id", pe.ID, "error", werr)
		}
	}
	progress := func(line string) {
		if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.SetExperimentProgress(ctx, pe.ID, line) }); err != nil {
			s.log.WarnContext(ctx, "experiment progress", "id", pe.ID, "error", err)
		}
	}

	progress(fmt.Sprintf("asking %s for %d variants", pe.OptimizerModel, pe.VariantCount))
	variants, err := s.generateVariants(ctx, &pe, subject)
	if err != nil {
		fail(err)
		return
	}
	candidates := append([]experimentCandidate{{Title: "baseline (unchanged)", Content: pe.Baseline, Baseline: true}}, variants...)

	progress(fmt.Sprintf("running baseline + %d variants on %s", len(variants), pe.TargetModel))
	s.runCandidates(ctx, subject, candidates)

	progress(fmt.Sprintf("judging %d outputs with %s", len(candidates), pe.OptimizerModel))
	results, err := s.judgeCandidates(ctx, &pe, candidates)
	if err != nil {
		fail(err)
		return
	}
	raw, err := json.Marshal(results)
	if err != nil {
		fail(err)
		return
	}
	err = s.store.Write(context.WithoutCancel(ctx), func(tx *store.Tx) error {
		return tx.FinishExperiment(ctx, pe.ID, store.ExperimentDone, raw, "")
	})
	if err != nil {
		s.log.ErrorContext(ctx, "record experiment results", "id", pe.ID, "error", err)
	}
}

// generateVariants asks the optimizer for candidate edits and validates each
// with the subject's own bar; an invalid variant is kept in the results with
// its refusal, not silently dropped.
func (s *Server) generateVariants(ctx context.Context, pe *store.Experiment, subject experimentSubject) ([]experimentCandidate, error) {
	var sb strings.Builder
	fmt.Fprintf(&sb, `You improve prompts for a system that runs coding agents. Produce exactly %d DISTINCT improved variants of the content below. Return ONLY JSON:
{"variants": [{"title": "<short name for the change>", "rationale": "<one sentence: what this tries and why it serves the goal>", "content": "<the COMPLETE replacement text>"}]}

GOAL (what better means here): %s
The variants will run on model %q — write for that model's strengths; a variant that only works on a larger model is a failed variant.

Rules:
- Each variant is the complete replacement content, not a diff.
- Vary meaningfully: different structure, emphasis, brevity, examples, guardrails — not %d synonyms of one idea.
- Keep what already works; the goal states the gap, not a rewrite mandate.
`, pe.VariantCount, pe.Goal, pe.TargetModel, pe.VariantCount)
	subject.generationRules(&sb)
	fmt.Fprintf(&sb, "\nCURRENT CONTENT (%s):\n%s\n", pe.Subject, pe.Baseline)
	if len(pe.Test) > 0 {
		fmt.Fprintf(&sb, "\nTHE TEST it will be judged on: %s\n", pe.Test)
	}

	raw, err := s.experimentModelCall(ctx, sb.String(), pe.OptimizerModel)
	if err != nil {
		return nil, fmt.Errorf("variant generation (%s): %w", pe.OptimizerModel, err)
	}
	var parsed struct {
		Variants []struct {
			Title     string `json:"title"`
			Rationale string `json:"rationale"`
			Content   string `json:"content"`
		} `json:"variants"`
	}
	if err := json.Unmarshal([]byte(extractJSON(raw)), &parsed); err != nil || len(parsed.Variants) == 0 {
		return nil, fmt.Errorf("the optimizer did not return variants")
	}
	var out []experimentCandidate
	for i, v := range parsed.Variants {
		c := experimentCandidate{Title: v.Title, Rationale: v.Rationale, Content: v.Content}
		if c.Title == "" {
			c.Title = fmt.Sprintf("variant %d", i+1)
		}
		if reason := subject.validate(c.Content); reason != "" {
			c.Error = reason
		}
		out = append(out, c)
	}
	return out, nil
}

// runCandidates executes every runnable candidate's test, a few at a time; a
// failed run lands in the candidate's error, never aborts the experiment.
func (s *Server) runCandidates(ctx context.Context, subject experimentSubject, candidates []experimentCandidate) {
	sem := make(chan struct{}, experimentRunners)
	var wg sync.WaitGroup
	for i := range candidates {
		if candidates[i].Error != "" {
			continue
		}
		wg.Add(1)
		go func(c *experimentCandidate) {
			defer wg.Done()
			sem <- struct{}{}
			defer func() { <-sem }()
			out, err := subject.run(ctx, c.Content, c.Baseline)
			if err != nil {
				c.Error = "run failed: " + err.Error()
				return
			}
			c.Output = out
		}(&candidates[i])
	}
	wg.Wait()
}

// judgeCandidates has the optimizer rank the outputs against the goal. The
// candidates are presented shuffled and unlabeled-as-baseline to blunt
// position and loyalty bias; the mapping comes back through opaque labels.
func (s *Server) judgeCandidates(ctx context.Context, pe *store.Experiment, candidates []experimentCandidate) (*experimentResults, error) {
	type entry struct {
		label string
		idx   int
	}
	var runnable []entry
	for i := range candidates {
		if candidates[i].Error == "" && candidates[i].Output != "" {
			runnable = append(runnable, entry{idx: i})
		}
	}
	if len(runnable) == 0 {
		return nil, fmt.Errorf("no candidate produced an output to judge")
	}
	rand.Shuffle(len(runnable), func(i, j int) { runnable[i], runnable[j] = runnable[j], runnable[i] })
	for i := range runnable {
		runnable[i].label = fmt.Sprintf("R%d", i+1)
	}

	var sb strings.Builder
	fmt.Fprintf(&sb, `You judge outputs produced by candidate prompts, all given the same test on model %q. Score each 0-10 against the goal, strictly on the OUTPUT text. Return ONLY JSON:
{"summary": "<2-3 sentences: what separated the strong from the weak>", "best": "<label>", "scores": [{"label": "<label>", "score": <0-10>, "rationale": "<one sentence>"}]}

GOAL: %s
THE TEST the candidates were given: %s

`, pe.TargetModel, pe.Goal, string(pe.Test))
	for _, e := range runnable {
		out := candidates[e.idx].Output
		if len(out) > judgeOutputCap {
			out = out[:judgeOutputCap] + "\n[truncated]"
		}
		fmt.Fprintf(&sb, "=== OUTPUT %s ===\n%s\n\n", e.label, out)
	}

	raw, err := s.experimentModelCall(ctx, sb.String(), pe.OptimizerModel)
	if err != nil {
		return nil, fmt.Errorf("judging (%s): %w", pe.OptimizerModel, err)
	}
	var verdict struct {
		Summary string `json:"summary"`
		Best    string `json:"best"`
		Scores  []struct {
			Label     string  `json:"label"`
			Score     float64 `json:"score"`
			Rationale string  `json:"rationale"`
		} `json:"scores"`
	}
	if err := json.Unmarshal([]byte(extractJSON(raw)), &verdict); err != nil {
		return nil, fmt.Errorf("the judge did not return scores")
	}
	byLabel := map[string]int{}
	for _, e := range runnable {
		byLabel[e.label] = e.idx
	}
	for _, sc := range verdict.Scores {
		if idx, ok := byLabel[sc.Label]; ok {
			candidates[idx].Score = sc.Score
			candidates[idx].JudgeRationale = sc.Rationale
		}
	}
	best := ""
	if idx, ok := byLabel[verdict.Best]; ok {
		best = candidates[idx].Title
	}
	sort.SliceStable(candidates, func(i, j int) bool {
		if (candidates[i].Error == "") != (candidates[j].Error == "") {
			return candidates[i].Error == ""
		}
		return candidates[i].Score > candidates[j].Score
	})
	return &experimentResults{Summary: verdict.Summary, Best: best, Candidates: candidates}, nil
}

// renderPreviewLib is renderPreview with an explicit library — variant
// composition without touching the daemon's loaded tree.
func (s *Server) renderPreviewLib(ctx context.Context, lib *directives.Library, rt store.Routine, objective, repo string) (routinePreview, error) {
	return s.renderPreviewOpts(ctx, rt, materializeOpts{Objective: objective, Lib: lib}, repo)
}

// abortExperiment is POST /api/v1/experiments/{id}/abort.
func (s *Server) abortExperiment(r *http.Request) (int, any, error) {
	ctx := r.Context()
	id := r.PathValue("id")
	res := &liveResults{Decision: string(store.ExperimentAborted), Reason: "operator abort"}
	b, _ := json.Marshal(res)
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		return tx.DecideLiveExperiment(ctx, id, store.ExperimentAborted, b, "")
	})
	if err != nil {
		return 0, nil, err
	}
	s.refreshLiveExperiments(ctx)
	s.log.InfoContext(ctx, "live experiment aborted", "id", id)
	return http.StatusOK, map[string]string{"id": id, "status": store.ExperimentAborted}, nil
}
