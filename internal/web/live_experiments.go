package web

// Live multivariant experiments (DESIGN.md §12): N arms of a directive or
// persona assigned to real production work at materialization, per-arm
// bookkeeping in attempt_facts, decision on the sweep tick, promotion
// guarded by the A/B revert net.
//
// The assignment cache is the load-bearing piece: per-arm libraries are
// built ONCE per (experiment, base library) via WithVariant — never inside
// the createWorkTx write transaction — and rebuilt whenever the base library
// pointer changes (the 30s hot reload, or a synchronous promptsReload), so
// an arm never composes against stale sibling fragments. Assignment itself
// is a map read plus an uncertainty-sampling pick under a mutex: the next
// run goes to the arm whose posterior is widest.
//
// Drift authority: the pinned control hash must equal the current library
// fragment's hash at every decide tick; any mismatch (human edit,
// reflect-library, an A/B revert) aborts the experiment. Cache suspension
// is only the soft half that stops serving arms in the ≤10s before the
// abort lands.

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strings"
	"time"

	"forge/internal/core/directives"
	"forge/internal/core/model"
	"forge/internal/core/stats"
	"forge/internal/core/store"
)

// liveArm is one arm ready to serve: lib is nil for control (the real
// library composes it).
type liveArm struct {
	label, title, hash, content string
	lib                         *directives.Library
}

// liveExperiment is one cached, serving experiment. counts are per-arm
// verified successes/failures from facts (reloaded every refresh); pending
// counts assignments made since the last refresh so consecutive picks spread
// before their outcomes land.
type liveExperiment struct {
	id, subjectKind, name string // subjectKind "directive" | "persona"
	arms                  []liveArm
	base                  *directives.Library
	counts                []armCount
	pending               []int
	suspended             string // non-empty reason => passthrough; the decide tick aborts
}

// armCount is one arm's outcome record, indexed like arms.
type armCount struct{ succ, fail int }

// experimentsLimits resolves [experiments] with defaults.
func (s *Engine) experimentsLimits() (minRuns, maxArms int, maxAge time.Duration, promote string) {
	minRuns, maxArms = s.experimentsCfg.MinRuns, s.experimentsCfg.MaxArms
	if minRuns <= 0 {
		minRuns = 5
	}
	if maxArms < 2 {
		maxArms = 3
	}
	days := s.experimentsCfg.MaxAgeDays
	if days <= 0 {
		days = 7
	}
	promote = s.experimentsCfg.Promote
	if promote != "propose" {
		promote = "auto"
	}
	return minRuns, maxArms, time.Duration(days) * 24 * time.Hour, promote
}

// refreshLiveExperiments rebuilds the assignment cache from the store and
// the current library. Entries whose experiment id and base library are
// unchanged are kept; per-arm outcome counts reload every refresh (and
// pending picks reset with them), so allocation follows the evidence.
func (s *Engine) refreshLiveExperiments(ctx context.Context) {
	lib := s.libraryNow()
	rows, err := s.store.LiveExperiments(ctx)
	if err != nil {
		s.log.WarnContext(ctx, "live experiments: refresh", "error", err)
		return
	}
	s.liveMu.Lock()
	old := map[string]*liveExperiment{}
	for _, e := range s.liveByDirective {
		old[e.id] = e
	}
	for _, e := range s.liveByPersona {
		old[e.id] = e
	}
	s.liveMu.Unlock()

	byDirective := map[string]*liveExperiment{}
	byPersona := map[string]*liveExperiment{}
	for i := range rows {
		row := &rows[i]
		if row.Status != store.ExperimentLive || lib == nil {
			continue
		}
		kind, name, ok := strings.Cut(row.Subject, ":")
		if !ok || (kind != "directive" && kind != "persona") {
			continue
		}
		entry := old[row.ID]
		if entry == nil || entry.base != lib {
			entry = s.buildLiveEntry(row, kind, name, lib)
		}
		if entry.suspended == "" {
			counts, err := s.armOutcomeCounts(ctx, row.ID, entry)
			if err != nil {
				s.log.WarnContext(ctx, "live experiments: arm counts", "experiment", row.ID, "error", err)
			} else {
				entry.counts, entry.pending = counts, make([]int, len(entry.arms))
			}
		}
		if kind == "directive" {
			byDirective[name] = entry
		} else {
			byPersona[name] = entry
		}
	}
	s.liveMu.Lock()
	s.liveByDirective, s.liveByPersona = byDirective, byPersona
	s.liveMu.Unlock()
}

// buildLiveEntry constructs one cache entry: per-arm libraries via
// WithVariant against base, suspended when the pinned control no longer
// matches the library or a variant no longer composes.
func (s *Engine) buildLiveEntry(row *store.Experiment, kind, name string, base *directives.Library) *liveExperiment {
	e := &liveExperiment{id: row.ID, subjectKind: kind, name: name, base: base}
	var arms []store.ExperimentArm
	if err := json.Unmarshal(row.Arms, &arms); err != nil || len(arms) < 2 {
		e.suspended = "arms unreadable"
		return e
	}
	frag := base.Fragment(name)
	if frag == nil || frag.Hash != arms[0].Hash {
		e.suspended = "drift"
		return e
	}
	for _, a := range arms {
		arm := liveArm{label: a.Label, title: a.Title, hash: a.Hash, content: a.Content}
		if a.Label != "control" {
			vlib, err := base.WithVariant(name, a.Content)
			if err != nil {
				e.suspended = fmt.Sprintf("arm %s no longer composes: %v", a.Label, err)
				return e
			}
			arm.lib = vlib
		}
		e.arms = append(e.arms, arm)
	}
	e.pending = make([]int, len(e.arms))
	return e
}

// assignLiveExperiment picks an arm for one materialization, or nothing.
// Precedence: a directive-subject experiment keyed by the target directive
// wins; else a persona-subject experiment keyed by the effective persona.
// At most one experiment per work; control returns a nil library (the base
// composes it) but still stamps.
func (s *Engine) assignLiveExperiment(rt *store.Routine, opts materializeOpts, base *directives.Library) (lib *directives.Library, expID, variant string) {
	if base == nil {
		return nil, "", ""
	}
	_, directiveName, _ := store.ParseTarget(rt.Target)
	persona := opts.Persona
	if persona == "" && directiveName != "" {
		if d := base.Directive(directiveName); d != nil {
			persona = d.PersonaRef
		}
	}
	if persona == "" {
		persona = rt.Persona
	}
	s.liveMu.Lock()
	defer s.liveMu.Unlock()
	entry := s.liveByDirective[directiveName]
	if entry == nil {
		entry = s.liveByPersona[persona]
	}
	if entry == nil || entry.suspended != "" || entry.base != base || len(entry.arms) == 0 {
		return nil, "", ""
	}
	// Uncertainty sampling: the next run goes to the arm whose posterior is
	// widest (Beta variance over its recorded outcomes), damped by picks
	// still in flight — information gain per run, not rotation. With no
	// outcomes yet this degenerates to spreading assignments evenly.
	best, bestScore := 0, -1.0
	for i := range entry.arms {
		var c armCount
		if i < len(entry.counts) {
			c = entry.counts[i]
		}
		pending := 0
		if i < len(entry.pending) {
			pending = entry.pending[i]
		}
		score := stats.BetaVariance(c.succ, c.fail) / float64(1+pending)
		if score > bestScore {
			best, bestScore = i, score
		}
	}
	if len(entry.pending) == len(entry.arms) {
		entry.pending[best]++
	}
	arm := entry.arms[best]
	return arm.lib, entry.id, arm.label
}

// liveExperimentTallies computes per-arm running tallies for one experiment
// (the GET surface). RateLow/RateHigh are the Wilson 95% bounds — at the
// small n live experiments run at, the interval is the honest number.
type armTally struct {
	Label        string  `json:"label"`
	Runs         int     `json:"runs"`
	VerifiedRate float64 `json:"verified_rate"`
	RateLow      float64 `json:"rate_low"`
	RateHigh     float64 `json:"rate_high"`
}

func (s *Engine) liveExperimentTallies(ctx context.Context, id string, minRuns int) ([]armTally, error) {
	facts, err := s.store.FactsByExperiment(ctx, id, 40*max(minRuns, 1))
	if err != nil {
		return nil, err
	}
	byArm := map[string][]store.AttemptFacts{}
	for _, f := range facts {
		byArm[f.Variant] = append(byArm[f.Variant], f)
	}
	var out []armTally
	for label, fs := range byArm {
		rate, _ := verifiedOutcome(fs)
		tl := armTally{Label: label, Runs: len(fs), VerifiedRate: rate}
		tl.RateLow, tl.RateHigh = stats.WilsonInterval(int(rate*float64(len(fs))+0.5), len(fs), 1.96)
		out = append(out, tl)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Label < out[j].Label })
	return out, nil
}

// hashContent is the arm hash for generated variants (control uses the
// library fragment's own hash).
func hashContent(content string) string {
	sum := sha256.Sum256([]byte(content))
	return hex.EncodeToString(sum[:])
}

// liveResults is the decision record stored on the experiment row.
type liveResults struct {
	Decision   string          `json:"decision"`
	Reason     string          `json:"reason"`
	Winner     string          `json:"winner,omitempty"`
	K          int             `json:"k"`
	Confidence float64         `json:"confidence,omitempty"` // required P(variant > control)
	Margin     float64         `json:"margin,omitempty"`     // legacy rows only
	Arms       []liveArmResult `json:"arms,omitempty"`
	ProposalID string          `json:"proposal_id,omitempty"`
	AppliedRef string          `json:"applied_ref,omitempty"`
}

type liveArmResult struct {
	Label            string   `json:"label"`
	Runs             int      `json:"runs"`
	VerifiedRate     float64  `json:"verified_rate"`
	PSuperiority     *float64 `json:"p_superiority,omitempty"` // P(this arm > control)
	CostPerSuccess   *float64 `json:"cost_per_success,omitempty"`
	MeanScoreOverall *float64 `json:"mean_score_overall,omitempty"`
}

// decideLiveExperiments is the sweep-tick pass: fail stale setups, abort on
// drift, and once every arm has an equal-n window, pick the winner and
// promote (or keep control / go inconclusive).
func (s *Engine) decideLiveExperiments(ctx context.Context) {
	rows, err := s.store.LiveExperiments(ctx)
	if err != nil {
		s.log.WarnContext(ctx, "live experiments: decide list", "error", err)
		return
	}
	lib := s.libraryNow()
	for i := range rows {
		row := &rows[i]
		if err := s.decideLiveExperiment(ctx, row, lib); err != nil {
			s.log.WarnContext(ctx, "live experiment decide", "experiment", row.ID, "subject", row.Subject, "error", err)
		}
	}
}

func (s *Engine) decideLiveExperiment(ctx context.Context, row *store.Experiment, lib *directives.Library) error {
	// A setup that died with the daemon holds the one-live-per-subject index
	// hostage; fail it like the offline stale rule does.
	if row.Status == store.ExperimentRunning {
		if time.Since(row.UpdatedAt) > experimentStaleAfter {
			return s.store.Write(ctx, func(tx *store.Tx) error {
				return tx.DecideLiveExperiment(ctx, row.ID, store.ExperimentFailed, nil, "abandoned setup — no progress (daemon restarted?)")
			})
		}
		return nil
	}
	_, name, _ := strings.Cut(row.Subject, ":")
	var arms []store.ExperimentArm
	if err := json.Unmarshal(row.Arms, &arms); err != nil || len(arms) < 2 {
		return s.closeLive(ctx, row, store.ExperimentFailed, &liveResults{Decision: "failed", Reason: "arms unreadable"}, "arms unreadable")
	}
	// Drift: the one rule — pinned control hash vs current fragment hash.
	if lib == nil || lib.Fragment(name) == nil || lib.Fragment(name).Hash != arms[0].Hash {
		res := &liveResults{Decision: string(store.ExperimentAborted), Reason: "drift: the subject changed under the experiment"}
		if err := s.closeLive(ctx, row, store.ExperimentAborted, res, ""); err != nil {
			return err
		}
		return s.store.Write(ctx, func(tx *store.Tx) error {
			return tx.Journal(ctx, "experiment.aborted", store.EntityDaemon, row.ID, map[string]any{"reason": "drift", "subject": row.Subject})
		})
	}
	minRuns := row.MinRuns
	if minRuns <= 0 {
		minRuns, _, _, _ = s.experimentsLimits()
	}
	facts, err := s.store.FactsByExperiment(ctx, row.ID, len(arms)*max(2*minRuns, 20))
	if err != nil {
		return err
	}
	byArm := map[string][]store.AttemptFacts{}
	for _, f := range facts { // newest first: the first minRuns per arm = equal-n windows
		if len(byArm[f.Variant]) < minRuns {
			byArm[f.Variant] = append(byArm[f.Variant], f)
		}
	}
	confidence := s.experimentsCfg.Confidence
	if confidence <= 0.5 || confidence >= 1 {
		confidence = 0.90
	}
	short := false
	results := &liveResults{K: minRuns, Confidence: confidence}
	succ := make([]int, len(arms))
	fail := make([]int, len(arms))
	for i, a := range arms {
		window := byArm[a.Label]
		ar := liveArmResult{Label: a.Label, Runs: len(window)}
		if len(window) > 0 {
			rate, cost := verifiedOutcome(window)
			ar.VerifiedRate, ar.CostPerSuccess = rate, cost
			ar.MeanScoreOverall = meanScoreOverall(window)
			succ[i] = int(rate*float64(len(window)) + 0.5)
			fail[i] = len(window) - succ[i]
		}
		results.Arms = append(results.Arms, ar)
		if len(window) < minRuns {
			short = true
		}
	}
	deadline := !row.DecideBy.IsZero() && s.now().After(row.DecideBy)
	if short {
		if deadline {
			results.Decision, results.Reason = string(store.ExperimentInconclusive), "deadline passed with insufficient runs"
			return s.closeLive(ctx, row, store.ExperimentInconclusive, results, "")
		}
		return nil // keep collecting
	}
	// Posterior decision: promote when some variant beats control with
	// P ≥ confidence; keep control when every variant is inferior with the
	// same confidence; otherwise keep collecting until the deadline. min_runs
	// is the floor, not the trigger — undecided evidence buys more evidence.
	winner, bestP := -1, 0.0
	allInferior := true
	for i := 1; i < len(results.Arms); i++ {
		p := stats.BetaSuperiority(succ[i], fail[i], succ[0], fail[0])
		pv := p
		results.Arms[i].PSuperiority = &pv
		if p > 1-confidence {
			allInferior = false
		}
		if p < confidence {
			continue
		}
		if winner < 0 || p > bestP || (p == bestP && betterArm(results.Arms[i], results.Arms[winner])) {
			winner, bestP = i, p
		}
	}
	if winner < 0 {
		if allInferior {
			results.Decision = string(store.ExperimentKeptControl)
			results.Reason = fmt.Sprintf("every variant inferior to control with ≥ %.0f%% probability", confidence*100)
			return s.closeLive(ctx, row, store.ExperimentKeptControl, results, "")
		}
		if deadline {
			results.Decision, results.Reason = string(store.ExperimentInconclusive), "deadline passed with the posterior undecided"
			return s.closeLive(ctx, row, store.ExperimentInconclusive, results, "")
		}
		return nil // undecided: keep collecting past min_runs
	}
	win := results.Arms[winner]
	results.Winner = win.Label
	results.Reason = fmt.Sprintf("%s beats control with probability %.2f (%.2f vs %.2f over ≥%d runs each)", win.Label, bestP, win.VerifiedRate, results.Arms[0].VerifiedRate, minRuns)
	var winContent string
	for _, a := range arms {
		if a.Label == win.Label {
			winContent = a.Content
		}
	}
	return s.promoteLiveWinner(ctx, row, name, winContent, results)
}

// betterArm orders two eligible arms: rate desc, cost/success asc (nil
// last), mean score desc.
func betterArm(a, b liveArmResult) bool {
	if a.VerifiedRate != b.VerifiedRate {
		return a.VerifiedRate > b.VerifiedRate
	}
	switch {
	case a.CostPerSuccess != nil && b.CostPerSuccess != nil && *a.CostPerSuccess != *b.CostPerSuccess:
		return *a.CostPerSuccess < *b.CostPerSuccess
	case a.CostPerSuccess != nil && b.CostPerSuccess == nil:
		return true
	case a.CostPerSuccess == nil && b.CostPerSuccess != nil:
		return false
	}
	as, bs := 0.0, 0.0
	if a.MeanScoreOverall != nil {
		as = *a.MeanScoreOverall
	}
	if b.MeanScoreOverall != nil {
		bs = *b.MeanScoreOverall
	}
	return as > bs
}

func meanScoreOverall(facts []store.AttemptFacts) *float64 {
	sum, n := 0, 0
	for _, f := range facts {
		if f.ScoreOverall != nil {
			sum += *f.ScoreOverall
			n++
		}
	}
	if n == 0 {
		return nil
	}
	m := float64(sum) / float64(n)
	return &m
}

// closeLive records a terminal decision and refreshes the cache so
// assignment stops immediately.
func (s *Engine) closeLive(ctx context.Context, row *store.Experiment, status string, results *liveResults, errMsg string) error {
	b, err := json.Marshal(results)
	if err != nil {
		return err
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error {
		return tx.DecideLiveExperiment(ctx, row.ID, status, b, errMsg)
	}); err != nil {
		return err
	}
	s.log.InfoContext(ctx, "live experiment decided", "experiment", row.ID, "subject", row.Subject, "status", status, "reason", results.Reason)
	s.refreshLiveExperiments(ctx)
	return nil
}

// promoteLiveWinner lands the winning arm through the proposal + apply
// discipline, so the A/B revert net guards the promoted commit like any
// other library edit. promote="propose" files the proposal and stops.
func (s *Engine) promoteLiveWinner(ctx context.Context, row *store.Experiment, name, content string, results *liveResults) error {
	_, _, _, promote := s.experimentsLimits()
	rationale, _ := json.Marshal(results.Arms)
	var pid string
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		p := &store.Proposal{
			Source: "experiment:" + row.ID, Kind: model.ProposalRoutine, Target: row.Subject,
			Before:           mustJSONRaw(map[string]string{"content": row.Baseline}),
			After:            mustJSONRaw(map[string]string{"prompt": content}),
			Rationale:        fmt.Sprintf("live experiment %s: %s — per-arm stats: %s", model.ShortID(row.ID), results.Reason, rationale),
			VerificationPlan: fmt.Sprintf("live A/B: auto-revert on regression vs pre-promotion runs (K=%d, confidence=%.2f)", results.K, results.Confidence),
		}
		if err := tx.CreateProposal(ctx, p); err != nil {
			return err
		}
		pid = p.ID
		if promote == "propose" {
			return nil
		}
		_, err := tx.DecideProposal(ctx, p.ID, model.ProposalApproved, "experiment:"+row.ID)
		return err
	})
	if err != nil {
		return err
	}
	results.ProposalID = pid
	if promote == "propose" {
		results.Decision, results.Reason = "winner_proposed", results.Reason+" — awaiting human apply"
		return s.closeLive(ctx, row, store.ExperimentDone, results, "")
	}
	ref, err := s.applyFragmentContent(ctx, name, content, "proposal:"+pid)
	if err != nil {
		results.Decision = string(store.ExperimentFailed)
		if cerr := s.closeLive(ctx, row, store.ExperimentFailed, results, "apply winner: "+err.Error()); cerr != nil {
			s.log.WarnContext(ctx, "close after failed promotion", "error", cerr)
		}
		return fmt.Errorf("apply winner: %w", err)
	}
	results.AppliedRef = ref
	results.Decision = string(store.ExperimentPromoted)
	if err := s.store.Write(ctx, func(tx *store.Tx) error {
		if _, werr := tx.MarkProposalApplied(ctx, pid, ref); werr != nil {
			return werr
		}
		// The promotion is a resolvable prediction: the winner holds unless
		// the A/B net reverts this proposal within the horizon. Its stated
		// probability is the posterior that promoted it — calibration will
		// say whether our confidence threshold is honest.
		var prob *float64
		for _, ar := range results.Arms {
			if ar.Label == results.Winner {
				prob = ar.PSuperiority
			}
		}
		return tx.InsertPrediction(ctx, &store.Prediction{
			Source: "experiment", SourceRef: row.ID, ProposalID: pid, Subject: row.Subject,
			Statement:   fmt.Sprintf("promoted %s of %s survives the A/B net", results.Winner, row.Subject),
			Probability: prob, ResolveBy: s.now().Add(7 * 24 * time.Hour),
		})
	}); err != nil {
		return err
	}
	return s.closeLive(ctx, row, store.ExperimentPromoted, results, "")
}

func mustJSONRaw(v any) json.RawMessage {
	b, err := json.Marshal(v)
	if err != nil {
		return json.RawMessage(`{}`)
	}
	return b
}

// runLiveSetup prepares a live experiment's arms in the background: source
// candidates (an offline experiment's results, or fresh generation),
// validate each against the CURRENT library, optionally judge-prescreen to
// rank, pin control, and open assignment.
func (s *Server) runLiveSetup(pe store.Experiment, subject experimentSubject, maxArms int, from string) {
	ctx, cancel := context.WithTimeout(context.Background(), experimentTotalTimeout)
	defer cancel()
	fail := func(err error) {
		s.log.WarnContext(ctx, "live experiment setup failed", "id", pe.ID, "error", err)
		if werr := s.store.Write(ctx, func(tx *store.Tx) error {
			return tx.DecideLiveExperiment(ctx, pe.ID, store.ExperimentFailed, nil, err.Error())
		}); werr != nil {
			s.log.WarnContext(ctx, "record live setup failure", "id", pe.ID, "error", werr)
		}
	}
	progress := func(line string) {
		if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.SetExperimentProgress(ctx, pe.ID, line) }); err != nil {
			s.log.DebugContext(ctx, "live setup progress", "error", err)
		}
	}

	var candidates []experimentCandidate
	if from != "" {
		src, err := s.store.GetExperiment(ctx, from)
		if err != nil {
			fail(fmt.Errorf("from experiment: %w", err))
			return
		}
		var res experimentResults
		if err := json.Unmarshal(src.Results, &res); err != nil {
			fail(fmt.Errorf("from experiment %s has no readable results", from))
			return
		}
		for _, c := range res.Candidates {
			if !c.Baseline && c.Error == "" && strings.TrimSpace(c.Content) != "" {
				candidates = append(candidates, c)
			}
		}
	} else {
		progress("asking " + pe.OptimizerModel + " for arm candidates")
		var err error
		candidates, err = s.generateVariants(ctx, &pe, subject)
		if err != nil {
			fail(err)
			return
		}
	}
	valid := candidates[:0]
	for _, c := range candidates {
		if msg := subject.validate(c.Content); msg == "" {
			valid = append(valid, c)
		}
	}
	if len(valid) == 0 {
		fail(fmt.Errorf("no candidate composes against the current library"))
		return
	}
	// Prescreen when there are more valid candidates than slots and a test
	// exists to judge them on.
	slots := maxArms - 1
	if len(valid) > slots && len(pe.Test) > 0 {
		progress("prescreening " + fmt.Sprint(len(valid)) + " candidates")
		s.runCandidates(ctx, subject, valid)
		if res, err := s.judgeCandidates(ctx, &pe, valid); err == nil {
			ranked := valid[:0]
			for _, c := range res.Candidates {
				if c.Error == "" && strings.TrimSpace(c.Content) != "" && !c.Baseline {
					ranked = append(ranked, c)
				}
			}
			if len(ranked) > 0 {
				valid = ranked
			}
		}
	}
	if len(valid) > slots {
		valid = valid[:slots]
	}

	// Pin control from the CURRENT library — the file may have moved since
	// the row was created.
	lib := s.promptLibrary()
	_, name, _ := strings.Cut(pe.Subject, ":")
	frag := lib.Fragment(name)
	if frag == nil {
		fail(fmt.Errorf("subject %s vanished from the library", pe.Subject))
		return
	}
	control, err := readFragmentFile(frag.Path)
	if err != nil {
		fail(err)
		return
	}
	arms := []store.ExperimentArm{{Label: "control", Content: control, Hash: frag.Hash}}
	for i, c := range valid {
		arms = append(arms, store.ExperimentArm{Label: fmt.Sprintf("v%d", i+1), Title: c.Title, Content: c.Content, Hash: hashContent(c.Content)})
	}
	armsJSON, err := json.Marshal(arms)
	if err != nil {
		fail(err)
		return
	}
	minRuns, _, maxAge, _ := s.experimentsLimits()
	if pe.MinRuns > 0 {
		minRuns = pe.MinRuns
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error {
		return tx.SetExperimentLive(ctx, pe.ID, control, armsJSON, minRuns, s.now().Add(maxAge))
	}); err != nil {
		fail(err)
		return
	}
	s.refreshLiveExperiments(ctx)
	s.log.InfoContext(ctx, "live experiment open", "id", pe.ID, "subject", pe.Subject, "arms", len(arms), "min_runs", minRuns)
}

// readFragmentFile is os.ReadFile with the error shaped for setup failures.
func readFragmentFile(path string) (string, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return "", fmt.Errorf("read subject file: %w", err)
	}
	return string(b), nil
}

// armOutcomeCounts reads one experiment's facts into per-arm verified
// success/failure counts, indexed like the entry's arms — the allocation
// evidence, reloaded every refresh.
func (s *Engine) armOutcomeCounts(ctx context.Context, id string, entry *liveExperiment) ([]armCount, error) {
	facts, err := s.store.FactsByExperiment(ctx, id, 200)
	if err != nil {
		return nil, err
	}
	byLabel := map[string]*armCount{}
	for _, f := range facts {
		c := byLabel[f.Variant]
		if c == nil {
			c = &armCount{}
			byLabel[f.Variant] = c
		}
		if f.VerificationPass != nil && *f.VerificationPass && model.IsSuccess(f.State) {
			c.succ++
		} else {
			c.fail++
		}
	}
	out := make([]armCount, len(entry.arms))
	for i, a := range entry.arms {
		if c := byLabel[a.label]; c != nil {
			out[i] = *c
		}
	}
	return out, nil
}
