package web

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"regexp"
	"strings"
	"sync"
	"testing"
	"time"

	"forge/internal/core/store"
)

// experimentFakeModel is a content-routed model seam: the generation prompt
// gets variants JSON, the judge prompt gets scores keyed off which candidate
// output each shuffled label carries, and everything else is a target-model
// run whose output identifies the candidate that produced it.
func experimentFakeModel(t *testing.T, variantsJSON string, outputFor func(prompt string) string, scoreFor func(output string) float64) func(context.Context, string, string, string) (string, error) {
	t.Helper()
	var mu sync.Mutex
	labelRe := regexp.MustCompile(`(?s)=== OUTPUT (R\d+) ===\n(.*?)\n\n`)
	return func(_ context.Context, _, user, model string) (string, error) {
		mu.Lock()
		defer mu.Unlock()
		switch {
		case strings.Contains(user, "DISTINCT improved variants"):
			return variantsJSON, nil
		case strings.Contains(user, "You judge outputs"):
			type score struct {
				Label     string  `json:"label"`
				Score     float64 `json:"score"`
				Rationale string  `json:"rationale"`
			}
			verdict := struct {
				Summary string  `json:"summary"`
				Best    string  `json:"best"`
				Scores  []score `json:"scores"`
			}{Summary: "the tighter one held up"}
			bestScore := -1.0
			for _, m := range labelRe.FindAllStringSubmatch(user, -1) {
				sc := score{Label: m[1], Score: scoreFor(m[2]), Rationale: "because"}
				verdict.Scores = append(verdict.Scores, sc)
				if sc.Score > bestScore {
					bestScore, verdict.Best = sc.Score, sc.Label
				}
			}
			raw, err := json.Marshal(verdict)
			return string(raw), err
		default:
			return outputFor(user), nil
		}
	}
}

func waitExperiment(t *testing.T, h *harness, id string) store.Experiment {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		var pe store.Experiment
		h.call(http.MethodGet, "/api/v1/experiments?id="+id, nil, &pe, http.StatusOK)
		if pe.Status != store.ExperimentRunning {
			return pe
		}
		time.Sleep(25 * time.Millisecond)
	}
	t.Fatal("experiment never finished")
	return store.Experiment{}
}

// The persona pipeline end to end: the optimizer's variants run alongside the
// untouched baseline, an invalid variant is kept with its refusal instead of
// silently dropped, the shuffled judge's scores map back to the right
// candidates, and the ranking puts the winner first with errored candidates
// last.
func TestExperimentPersona(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{
		"personas/reviewer.md": "---\nmodel: haiku\n---\nYou are the reviewer.\n\n## mode: run\nRun teaching.",
	})

	variants := `{"variants": [
		{"title": "tighter", "rationale": "less filler", "content": "---\nmodel: haiku\n---\nTighter reviewer.\n\n## mode: run\nGo."},
		{"title": "broken", "rationale": "oops", "content": "{{> does-not-exist}}"}
	]}`
	h.srv.modelCall = experimentFakeModel(t, variants,
		func(prompt string) string {
			if strings.Contains(prompt, "Tighter reviewer.") {
				return "OUT-TIGHT"
			}
			return "OUT-BASE"
		},
		func(output string) float64 {
			if strings.Contains(output, "OUT-TIGHT") {
				return 9
			}
			return 4
		})

	var created struct {
		ID string `json:"id"`
	}
	h.call(http.MethodPost, "/api/v1/experiments", map[string]any{
		"subject": "persona:reviewer", "goal": "be terser",
		"target_model": "haiku", "optimizer_model": "sonnet",
		"test": map[string]string{"mode": "run", "task": "Fix {{repo}}: {{objective}}", "objective": "the gauges", "repo": "equitizr"},
	}, &created, http.StatusCreated)

	pe := waitExperiment(t, h, created.ID)
	if pe.Status != store.ExperimentDone || pe.Error != "" {
		t.Fatalf("experiment = %s error=%q", pe.Status, pe.Error)
	}
	if !strings.Contains(pe.Baseline, "You are the reviewer.") {
		t.Errorf("baseline not captured: %q", pe.Baseline)
	}

	var results experimentResults
	if err := json.Unmarshal(pe.Results, &results); err != nil {
		t.Fatalf("results: %v", err)
	}
	if results.Best != "tighter" || results.Summary == "" {
		t.Errorf("best = %q summary = %q", results.Best, results.Summary)
	}
	if len(results.Candidates) != 3 {
		t.Fatalf("candidates = %d", len(results.Candidates))
	}
	win, base, bad := results.Candidates[0], results.Candidates[1], results.Candidates[2]
	if win.Title != "tighter" || win.Score != 9 || win.Output != "OUT-TIGHT" || win.Baseline {
		t.Errorf("winner = %+v", win)
	}
	if !base.Baseline || base.Score != 4 || base.Output != "OUT-BASE" {
		t.Errorf("baseline candidate = %+v", base)
	}
	if bad.Title != "broken" || !strings.Contains(bad.Error, "does not compose") || bad.Output != "" {
		t.Errorf("invalid variant = %+v", bad)
	}

	// The subject listing carries it, newest first.
	var list []store.Experiment
	h.call(http.MethodGet, "/api/v1/experiments?subject=persona:reviewer", nil, &list, http.StatusOK)
	if len(list) != 1 || list[0].ID != created.ID {
		t.Errorf("list = %+v", list)
	}
}

// Refusals happen before any model call: no seam, no goal, bad alias, bad
// subject shapes.
func TestExperimentRefusals(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{"personas/reviewer.md": "---\nmodel: haiku\n---\nHi."})

	ok := map[string]any{"subject": "persona:reviewer", "goal": "g", "target_model": "haiku", "optimizer_model": "sonnet"}
	if status, body := h.do(http.MethodPost, "/api/v1/experiments", ok, nil, ""); status != http.StatusBadRequest || !strings.Contains(string(body), "model access") {
		t.Fatalf("no seam = %d %s", status, body)
	}
	h.srv.modelCall = func(_ context.Context, _, _, _ string) (string, error) {
		t.Error("model called on a refused request")
		return "", nil
	}
	for name, req := range map[string]map[string]any{
		"no goal":         {"subject": "persona:reviewer", "target_model": "haiku", "optimizer_model": "sonnet"},
		"bad alias":       {"subject": "persona:reviewer", "goal": "g", "target_model": "bogus", "optimizer_model": "sonnet"},
		"bare subject":    {"subject": "reviewer", "goal": "g", "target_model": "haiku", "optimizer_model": "sonnet"},
		"unknown kind":    {"subject": "widget:reviewer", "goal": "g", "target_model": "haiku", "optimizer_model": "sonnet"},
		"unknown persona": {"subject": "persona:nobody", "goal": "g", "target_model": "haiku", "optimizer_model": "sonnet"},
	} {
		if status, _ := h.do(http.MethodPost, "/api/v1/experiments", req, nil, ""); status != http.StatusBadRequest {
			t.Errorf("%s = %d, want 400", name, status)
		}
	}
	if status, _ := h.do(http.MethodGet, "/api/v1/experiments", nil, nil, ""); status != http.StatusBadRequest {
		t.Errorf("list without subject = %d", status)
	}
}

// An optimizer that refuses to produce variants fails the experiment with a
// recorded error instead of leaving it running.
func TestExperimentGenerationFailure(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{"personas/reviewer.md": "---\nmodel: haiku\n---\nHi."})
	h.srv.modelCall = func(_ context.Context, _, user, _ string) (string, error) {
		return "I would rather not.", nil
	}
	var created struct {
		ID string `json:"id"`
	}
	h.call(http.MethodPost, "/api/v1/experiments", map[string]any{
		"subject": "persona:reviewer", "goal": "g", "target_model": "haiku", "optimizer_model": "sonnet",
	}, &created, http.StatusCreated)
	pe := waitExperiment(t, h, created.ID)
	if pe.Status != store.ExperimentFailed || !strings.Contains(pe.Error, "did not return variants") {
		t.Errorf("experiment = %s error=%q", pe.Status, pe.Error)
	}
}

// A running row that stops updating (daemon restarted under the worker) reads
// back as failed once stale.
func TestExperimentStale(t *testing.T) {
	h := newHarness(t, transportUnix)
	ctx := context.Background()
	pe := store.Experiment{Subject: "persona:reviewer", Goal: "g", TargetModel: "haiku", OptimizerModel: "sonnet", VariantCount: 8}
	if err := h.srv.store.Write(ctx, func(tx *store.Tx) error { return tx.InsertExperiment(ctx, &pe) }); err != nil {
		t.Fatal(err)
	}
	var got store.Experiment
	h.call(http.MethodGet, "/api/v1/experiments?id="+pe.ID, nil, &got, http.StatusOK)
	if got.Status != store.ExperimentRunning {
		t.Fatalf("fresh row = %s", got.Status)
	}
	h.clock.Advance(experimentStaleAfter + time.Minute)
	h.call(http.MethodGet, "/api/v1/experiments?id="+pe.ID, nil, &got, http.StatusOK)
	if got.Status != store.ExperimentFailed || !strings.Contains(got.Error, "abandoned") {
		t.Errorf("stale row = %s error=%q", got.Status, got.Error)
	}
}

// The per-subject trim keeps history bounded.
func TestExperimentTrim(t *testing.T) {
	h := newHarness(t, transportUnix)
	ctx := context.Background()
	for i := 0; i < 8; i++ {
		pe := store.Experiment{Subject: "persona:reviewer", Goal: fmt.Sprintf("g%d", i), TargetModel: "haiku", OptimizerModel: "sonnet", VariantCount: 1}
		if err := h.srv.store.Write(ctx, func(tx *store.Tx) error { return tx.InsertExperiment(ctx, &pe) }); err != nil {
			t.Fatal(err)
		}
		h.clock.Advance(time.Second)
	}
	var list []store.Experiment
	h.call(http.MethodGet, "/api/v1/experiments?subject=persona:reviewer", nil, &list, http.StatusOK)
	if len(list) != 5 {
		t.Fatalf("kept %d, want 5", len(list))
	}
	if list[0].Goal != "g7" {
		t.Errorf("newest first: %q", list[0].Goal)
	}
}

// The directive subject: variants are whole directive files, validated by
// library composition, run through the real assembly; a trigger routine as a
// subject is refused with a pointer at the directive.
func TestExperimentDirective(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{
		"directives/triage.md": "---\nmode: run\nmodel: haiku\n---\nOld triage: {{objective}}",
	})
	variants := `{"variants": [
		{"title": "sharper", "rationale": "r", "content": "---\nmode: run\nmodel: haiku\n---\nNew triage: {{objective}}"},
		{"title": "broken", "rationale": "r", "content": "---\nmode: run\n---\n{{> ghost}}"}
	]}`
	h.srv.modelCall = experimentFakeModel(t, variants,
		func(prompt string) string {
			if strings.Contains(prompt, "New triage: obj") {
				return "OUT-NEW"
			}
			return "OUT-OLD"
		},
		func(output string) float64 {
			if strings.Contains(output, "OUT-NEW") {
				return 9
			}
			return 3
		})

	var created struct {
		ID string `json:"id"`
	}
	h.call(http.MethodPost, "/api/v1/experiments", map[string]any{
		"subject": "directive:triage", "goal": "sharper triage",
		"target_model": "haiku", "optimizer_model": "sonnet",
		"test": map[string]string{"objective": "obj", "repo": "equitizr"},
	}, &created, http.StatusCreated)
	pe := waitExperiment(t, h, created.ID)
	if pe.Status != store.ExperimentDone {
		t.Fatalf("experiment = %s error=%q", pe.Status, pe.Error)
	}
	if !strings.Contains(pe.Baseline, "Old triage") {
		t.Errorf("baseline = %q", pe.Baseline)
	}
	var results experimentResults
	if err := json.Unmarshal(pe.Results, &results); err != nil {
		t.Fatal(err)
	}
	if results.Best != "sharper" || results.Candidates[0].Output != "OUT-NEW" {
		t.Errorf("results = %+v", results)
	}
	last := results.Candidates[len(results.Candidates)-1]
	if last.Title != "broken" || !strings.Contains(last.Error, "does not compose") {
		t.Errorf("broken variant = %+v", last)
	}

	// A trigger routine is not an experiment subject anymore.
	rt := store.Routine{Name: "triage-trigger", Target: "directive:triage", Repositories: []string{"equitizr"}}
	h.call(http.MethodPost, "/api/v1/routines", rt, nil, http.StatusCreated)
	if status, body := h.do(http.MethodPost, "/api/v1/experiments", map[string]any{
		"subject": "routine:triage-trigger", "goal": "g", "target_model": "haiku", "optimizer_model": "sonnet",
	}, nil, ""); status != http.StatusBadRequest || !strings.Contains(string(body), "directive:triage") {
		t.Errorf("trigger subject = %d %s", status, body)
	}
}
