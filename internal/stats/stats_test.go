package stats_test

import (
	"context"
	"encoding/json"
	"fmt"
	"math"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/model"
	"forge/internal/protocol"
	"forge/internal/stats"
	"forge/internal/store"
)

func ctx() context.Context { return context.Background() }

func iptr(v int) *int         { return &v }
func i64(v int64) *int64      { return &v }
func fptr(v float64) *float64 { return &v }
func bptr(v bool) *bool       { return &v }

// fact is the base synthetic row; tests fill only the fields under test —
// everything else stays NULL, exactly like a real sparse facts row.
func fact(routine string, gen int, state model.State) store.AttemptFacts {
	return store.AttemptFacts{Routine: routine, Generation: gen, State: state,
		Repository: "repo", Project: "default", Mode: "run"}
}

func approx(t *testing.T, name string, got, want float64) {
	t.Helper()
	if math.Abs(got-want) > 1e-9 {
		t.Errorf("%s = %v, want %v", name, got, want)
	}
}

// TestCompute drives the pure aggregation over 6 rows across 2 routines × 2
// generations with mixed outcomes and NULL columns, plus one prev-window row.
func TestCompute(t *testing.T) {
	r1 := fact("alpha", 1, model.Succeeded) // the one verified success of alpha
	r1.VerificationPass, r1.IsError = bptr(true), bptr(false)
	r1.Phases = map[string]*int64{"total": i64(100), "agent": i64(80)}
	r1.InputTokens, r1.OutputTokens = i64(1000), i64(100)
	r1.CostUSD, r1.Turns, r1.QuestionsAsked = fptr(1.0), iptr(10), iptr(1)
	r1.ToolCallsByName = map[string]int{"Bash": 2, "Read": 1}
	r1.UtilizationDelta = fptr(0.05)

	r2 := fact("alpha", 1, model.Failed) // NULL tokens, cost, turns
	r2.FailureReason, r2.IsError = model.ReasonTimeout, bptr(true)
	r2.Phases = map[string]*int64{"total": i64(400), "agent": i64(300)}
	r2.ToolCallsByName = map[string]int{"Bash": 1}
	r2.UtilizationDelta = fptr(-0.02) // a reset boundary: never consumed

	r3 := fact("alpha", 2, model.Unverified) // claimed success, verification failed
	r3.VerificationPass, r3.IsError = bptr(false), bptr(false)
	r3.Phases = map[string]*int64{"total": i64(200), "agent": i64(150)}
	r3.CostUSD = fptr(0.5)
	r3.ToolCallsByName = map[string]int{"Read": 3}
	r3.UtilizationDelta = fptr(0.01)

	r4 := fact("alpha", 2, model.Cancelled) // everything NULL, even the phases

	r5 := fact("beta", 1, model.Succeeded)
	r5.VerificationPass, r5.IsError = bptr(true), bptr(false)
	r5.Phases = map[string]*int64{"total": i64(300)}
	r5.CostUSD = fptr(2.0)

	r6 := fact("beta", 2, model.Failed)
	r6.FailureReason, r6.IsError = model.ReasonExitNonzero, bptr(true)
	r6.Phases = map[string]*int64{"total": i64(500)}

	p1 := fact("alpha", 1, model.Succeeded)
	p1.VerificationPass, p1.CostUSD = bptr(true), fptr(4.0)

	r := stats.Compute([]store.AttemptFacts{r1, r2, r3, r4, r5, r6}, []store.AttemptFacts{p1})

	if r.TotalRuns != 6 {
		t.Errorf("TotalRuns = %d, want 6", r.TotalRuns)
	}
	if len(r.Routines) != 2 || r.Routines[0].Routine != "alpha" || r.Routines[1].Routine != "beta" {
		t.Fatalf("Routines = %+v, want alpha then beta", r.Routines)
	}

	alpha := r.Routines[0]
	if alpha.Generation != 0 {
		t.Errorf("rollup generation = %d, want 0", alpha.Generation)
	}
	if alpha.Runs != 4 {
		t.Errorf("alpha runs = %d, want 4", alpha.Runs)
	}
	wantOutcomes := map[string]int{"succeeded": 1, "failed": 1, "unverified": 1, "cancelled": 1}
	if fmt.Sprint(alpha.Outcomes) != fmt.Sprint(wantOutcomes) {
		t.Errorf("alpha outcomes = %v, want %v", alpha.Outcomes, wantOutcomes)
	}
	if alpha.VerifiedSuccesses != 1 {
		t.Errorf("alpha verified successes = %d, want 1", alpha.VerifiedSuccesses)
	}
	// Verified rate: 1 verified success over all 4 runs.
	approx(t, "alpha verified rate", alpha.VerifiedSuccessRate, 0.25)
	// Self-reported rate: is_error is non-NULL on 3 runs, false on 2 of them
	// (r4's NULL is excluded from the denominator).
	approx(t, "alpha self-reported rate", alpha.SelfReportedSuccessRate, 2.0/3.0)
	// Nearest-rank over the 3 rows with a total phase (r4 has none):
	// sorted totals [100 200 400], ⌈0.5·3⌉=2nd, ⌈0.95·3⌉=3rd.
	if alpha.P50TotalUS != 200 || alpha.P95TotalUS != 400 || alpha.MaxTotalUS != 400 {
		t.Errorf("alpha total percentiles = %d/%d/%d, want 200/400/400", alpha.P50TotalUS, alpha.P95TotalUS, alpha.MaxTotalUS)
	}
	if alpha.P50AgentUS != 150 || alpha.P95AgentUS != 300 || alpha.MaxAgentUS != 300 {
		t.Errorf("alpha agent percentiles = %d/%d/%d, want 150/300/300", alpha.P50AgentUS, alpha.P95AgentUS, alpha.MaxAgentUS)
	}
	// Only r1 carries tokens and turns: the mean is over that one run.
	approx(t, "alpha tokens in/run", alpha.TokensInPerRun, 1000)
	approx(t, "alpha tokens out/run", alpha.TokensOutPerRun, 100)
	approx(t, "alpha turns/run", alpha.TurnsPerRun, 10)
	approx(t, "alpha questions/run", alpha.QuestionsPerRun, 1)
	// Cost: 2 rows carry cost_usd (1.0 + 0.5); the NULL rows are excluded
	// from the per-run denominator, not counted as free.
	approx(t, "alpha cost total", alpha.CostUSDTotal, 1.5)
	approx(t, "alpha cost/run", alpha.CostPerRun, 0.75)
	if alpha.CostPerVerifiedSuccess == nil {
		t.Error("alpha cost per verified success = nil, want 1.5")
	} else {
		approx(t, "alpha cost/verified", *alpha.CostPerVerifiedSuccess, 1.5)
	}
	wantMix := map[string]int{"Bash": 3, "Read": 4}
	if fmt.Sprint(alpha.ToolMix) != fmt.Sprint(wantMix) {
		t.Errorf("alpha tool mix = %v, want %v", alpha.ToolMix, wantMix)
	}
	if len(alpha.TopFailureReasons) != 1 || alpha.TopFailureReasons[0] != (stats.ReasonCount{Reason: "timeout", Count: 1}) {
		t.Errorf("alpha failure reasons = %v", alpha.TopFailureReasons)
	}
	// 0.05 + 0.01; the −0.02 reset delta is skipped.
	approx(t, "alpha utilization consumed", alpha.UtilizationConsumed, 0.06)

	beta := r.Routines[1]
	if beta.CostPerVerifiedSuccess == nil {
		t.Error("beta cost per verified success = nil, want 2.0")
	} else {
		approx(t, "beta cost/verified", *beta.CostPerVerifiedSuccess, 2.0)
	}

	// Per-generation split, sorted by routine then generation.
	var gens []string
	for _, g := range r.Generations {
		gens = append(gens, stats.Key(g.Routine, g.Generation))
	}
	if fmt.Sprint(gens) != fmt.Sprint([]string{"alpha@1", "alpha@2", "beta@1", "beta@2"}) {
		t.Fatalf("Generations = %v", gens)
	}
	a1, a2 := r.Generations[0], r.Generations[1]
	if a1.Runs != 2 || a2.Runs != 2 {
		t.Errorf("alpha generation runs = %d/%d, want 2/2", a1.Runs, a2.Runs)
	}
	// No verified success in alpha@2 (unverified + cancelled): the ratio is
	// undefined, so nil — never zero.
	if a2.CostPerVerifiedSuccess != nil {
		t.Errorf("alpha@2 cost per verified success = %v, want nil", *a2.CostPerVerifiedSuccess)
	}
	b2 := r.Generations[3]
	if b2.P50TotalUS != 500 || b2.P95TotalUS != 500 || b2.MaxTotalUS != 500 {
		t.Errorf("beta@2 percentiles over one value = %d/%d/%d, want 500", b2.P50TotalUS, b2.P95TotalUS, b2.MaxTotalUS)
	}
	if b2.P50AgentUS != 0 || b2.MaxAgentUS != 0 {
		t.Errorf("beta@2 agent percentiles = %d/%d, want 0 (no agent phase recorded)", b2.P50AgentUS, b2.MaxAgentUS)
	}

	// Prev wiring: the one prev-window row yields exactly its rollup and
	// generation keys; routines absent from the prev window have no entry.
	if len(r.Prev) != 2 {
		t.Fatalf("Prev keys = %v, want alpha and alpha@1", r.Prev)
	}
	if p, ok := r.Prev["alpha"]; !ok || p.Runs != 1 || p.VerifiedSuccessRate != 1 {
		t.Errorf(`Prev["alpha"] = %+v (ok=%v)`, p, ok)
	}
	if p, ok := r.Prev["alpha@1"]; !ok || p.Generation != 1 {
		t.Errorf(`Prev["alpha@1"] = %+v (ok=%v)`, p, ok)
	}
	if _, ok := r.Prev["beta"]; ok {
		t.Error(`Prev["beta"] present, want omitted (no prev runs)`)
	}
}

// TestComputeNearestRank pins the nearest-rank definition on ten known values.
func TestComputeNearestRank(t *testing.T) {
	var rows []store.AttemptFacts
	for i := int64(1); i <= 10; i++ {
		f := fact("p", 1, model.Failed)
		f.Phases = map[string]*int64{"total": i64(i * 10)}
		rows = append(rows, f)
	}
	r := stats.Compute(rows, nil)
	p := r.Routines[0]
	// ⌈0.5·10⌉ = 5th = 50; ⌈0.95·10⌉ = 10th = 100.
	if p.P50TotalUS != 50 || p.P95TotalUS != 100 || p.MaxTotalUS != 100 {
		t.Errorf("percentiles = %d/%d/%d, want 50/100/100", p.P50TotalUS, p.P95TotalUS, p.MaxTotalUS)
	}
}

// TestComputeTopFailureReasons proves the order (count desc, then reason) and
// the cap of 5.
func TestComputeTopFailureReasons(t *testing.T) {
	counts := map[model.FailureReason]int{
		model.ReasonTimeout: 3, model.ReasonExitNonzero: 3,
		model.ReasonInternal: 2, model.ReasonLaunchFailed: 2,
		model.ReasonWorktreeLost: 1, model.ReasonCancelled: 1, model.ReasonPrepareFailed: 1,
	}
	var rows []store.AttemptFacts
	for reason, n := range counts {
		for range n {
			f := fact("r", 1, model.Failed)
			f.FailureReason = reason
			rows = append(rows, f)
		}
	}
	got := stats.Compute(rows, nil).Routines[0].TopFailureReasons
	want := []stats.ReasonCount{
		{Reason: "exit_nonzero", Count: 3}, {Reason: "timeout", Count: 3},
		{Reason: "internal", Count: 2}, {Reason: "launch_failed", Count: 2},
		{Reason: "cancelled", Count: 1},
	}
	if fmt.Sprint(got) != fmt.Sprint(want) {
		t.Errorf("TopFailureReasons = %v\nwant %v", got, want)
	}
}

const workerID = "0123456789abcdef0123456789abcdef"

// fixture is the minimal daemon-side world (the shape of
// tools/tools_test.go): a real store with one project, one worker with one
// repository, and one routine.
type fixture struct {
	t   *testing.T
	s   *store.Store
	now time.Time
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	f := &fixture{t: t, now: time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)}
	s, err := store.Open(ctx(), filepath.Join(t.TempDir(), "forge.sqlite3"), store.Options{Clock: func() time.Time { return f.now }})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := s.Close(); err != nil {
			t.Error(err)
		}
	})
	f.s = s
	f.write(func(tx *store.Tx) error {
		if err := tx.EnsureProject(ctx(), "default"); err != nil {
			return err
		}
		if err := tx.Register(ctx(), protocol.RegisterRequest{WorkerID: workerID, Name: "laptop", Version: "test", MaxConcurrent: 2, Executors: []string{"claude-code"},
			Repositories: []protocol.Repository{{Name: "equitizr", Path: "/tmp/equitizr", OriginIdentity: "github.com/x/equitizr"}}}); err != nil {
			return err
		}
		return tx.CreateRoutine(ctx(), &store.Routine{Name: "inventory", Mode: "run", Prompt: "list files", Repositories: []string{"equitizr"}, Model: "haiku", TimeoutSeconds: 300, Schedule: "0 3 * * *", AllowedTools: []string{"Bash"}})
	})
	return f
}

func (f *fixture) write(fn func(tx *store.Tx) error) {
	f.t.Helper()
	if err := f.s.Write(ctx(), fn); err != nil {
		f.t.Fatal(err)
	}
}

// attempt creates a Work, claims its Target, and heartbeats it to running.
func (f *fixture) attempt(req string) *store.Attempt {
	f.t.Helper()
	f.now = f.now.Add(time.Second)
	w := &store.Work{RoutineName: "inventory", Generation: 1, Title: "inventory " + req, Trigger: model.TriggerManual, Snapshot: []byte(`{}`), Priority: 100, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto}
	var targets []store.Target
	f.write(func(tx *store.Tx) error {
		var err error
		targets, err = tx.CreateWork(ctx(), w, []string{"equitizr"}, nil)
		return err
	})
	var a *store.Attempt
	f.write(func(tx *store.Tx) error {
		var err error
		a, err = tx.Claim(ctx(), store.ClaimParams{TargetID: targets[0].ID, WorkerID: workerID, ClaimRequestID: req, LeaseToken: "lease-" + req, MCPToken: "mcp-" + req,
			Executor: "claude-code", Model: "claude-haiku-4-5", ModelAlias: "haiku", Mode: "run", Autonomy: model.AutonomyAuto})
		return err
	})
	for _, st := range []model.State{model.Preparing, model.Running} {
		f.write(func(tx *store.Tx) error {
			_, err := tx.RecordHeartbeat(ctx(), a.ID, protocol.HeartbeatRequest{LeaseToken: "lease-" + req, State: st})
			return err
		})
	}
	return a
}

// facts builds a minimal in-window facts row for one attempt.
func (f *fixture) facts(a *store.Attempt, state model.State, finished time.Time) *store.AttemptFacts {
	return &store.AttemptFacts{AttemptID: a.ID, TargetID: a.TargetID, Routine: "inventory", Generation: 1, Project: "default",
		Repository: "equitizr", Worker: workerID, Executor: "claude-code", Model: "haiku", Mode: "run", Trigger: model.TriggerManual, Autonomy: model.AutonomyAuto,
		FinishedAt: finished, State: state}
}

// TestLoadAndRetroPack exercises the store-backed path end to end: a failed
// and retained attempt with more than eventTail non-line events, a cancelled
// attempt, a verified success, and a previous-window row for Prev.
func TestLoadAndRetroPack(t *testing.T) {
	f := newFixture(t)
	aFail := f.attempt("f1")
	aCancel := f.attempt("c1")
	aOK := f.attempt("ok1")
	aPrev := f.attempt("p1")
	now := f.now

	// Complete the failed attempt with a retained worktree, an oversized
	// result text, and a structured result; cancel the second.
	f.write(func(tx *store.Tx) error {
		_, err := tx.Complete(ctx(), aFail.ID, protocol.CompleteRequest{LeaseToken: "lease-f1", State: model.Failed,
			FailureReason: model.ReasonExitNonzero, ExitCode: 1, IsError: true, NumTurns: 4, Launches: 1,
			ResultText: strings.Repeat("x", 2500), Result: json.RawMessage(`{"outcome":"failed"}`),
			Cleanup: protocol.Cleanup{Outcome: "retained", Reason: "debug"}, StartedAt: now.Add(-2 * time.Hour), FinishedAt: now.Add(-time.Hour)}, 1)
		return err
	})
	f.write(func(tx *store.Tx) error {
		_, err := tx.Complete(ctx(), aCancel.ID, protocol.CompleteRequest{LeaseToken: "lease-c1", State: model.Cancelled,
			FailureReason: model.ReasonCancelled, Cleanup: protocol.Cleanup{Outcome: "removed", Reason: "clean"},
			StartedAt: now.Add(-3 * time.Hour), FinishedAt: now.Add(-2 * time.Hour)}, 1)
		return err
	})
	// 35 non-line events plus 3 stdout lines at the end of the timeline: the
	// pack must keep the LAST 30 non-line events (seq 5..34) and never a line.
	var events []protocol.Event
	for i := 0; i < 35; i++ {
		events = append(events, protocol.Event{Seq: i, Time: now, ElapsedUS: int64(i), Kind: protocol.KindLifecycle, Message: fmt.Sprintf("step %d", i)})
	}
	for i := 35; i < 38; i++ {
		events = append(events, protocol.Event{Seq: i, Time: now, ElapsedUS: int64(1000 + i), Kind: protocol.KindStdout, Message: "line"})
	}
	f.write(func(tx *store.Tx) error {
		_, err := tx.InsertEvents(ctx(), aFail.ID, protocol.SourceWorker, events)
		return err
	})

	f.write(func(tx *store.Tx) error {
		fail := f.facts(aFail, model.Failed, now.Add(-time.Hour))
		fail.FailureReason, fail.IsError, fail.Retained, fail.RetainedReason = model.ReasonExitNonzero, bptr(true), true, "debug"
		if err := tx.InsertFacts(ctx(), fail); err != nil {
			return err
		}
		cancel := f.facts(aCancel, model.Cancelled, now.Add(-2*time.Hour))
		cancel.FailureReason = model.ReasonCancelled
		if err := tx.InsertFacts(ctx(), cancel); err != nil {
			return err
		}
		ok := f.facts(aOK, model.Succeeded, now.Add(-3*time.Hour))
		ok.VerificationPass, ok.IsError = bptr(true), bptr(false)
		if err := tx.InsertFacts(ctx(), ok); err != nil {
			return err
		}
		// One row in the previous equal window: [now−48h, now−24h).
		prev := f.facts(aPrev, model.Failed, now.Add(-30*time.Hour))
		prev.FailureReason = model.ReasonTimeout
		return tx.InsertFacts(ctx(), prev)
	})

	q := stats.Query{Since: now.Add(-24 * time.Hour), Until: now.Add(time.Minute)}
	report, err := stats.Load(ctx(), f.s, q)
	if err != nil {
		t.Fatal(err)
	}
	if report.TotalRuns != 3 || len(report.Routines) != 1 {
		t.Fatalf("TotalRuns = %d, routines = %d, want 3 runs of 1 routine", report.TotalRuns, len(report.Routines))
	}
	inv := report.Routines[0]
	if inv.Runs != 3 || inv.Outcomes["failed"] != 1 || inv.Outcomes["cancelled"] != 1 || inv.Outcomes["succeeded"] != 1 {
		t.Errorf("aggregate = %+v", inv)
	}
	if p, present := report.Prev["inventory"]; !present || p.Runs != 1 || p.TopFailureReasons[0].Reason != "timeout" {
		t.Errorf(`Prev["inventory"] = %+v (present=%v)`, p, present)
	}
	if filtered, err := stats.Load(ctx(), f.s, stats.Query{Since: q.Since, Until: q.Until, Mode: "weird"}); err != nil || filtered.TotalRuns != 0 {
		t.Errorf("mode-filtered TotalRuns = %v (err=%v), want 0", filtered.TotalRuns, err)
	}
	if _, err := stats.Load(ctx(), f.s, stats.Query{Since: q.Until, Until: q.Since}); err == nil {
		t.Error("inverted window accepted, want an error")
	}

	pack, err := stats.LoadRetroPack(ctx(), f.s, q)
	if err != nil {
		t.Fatal(err)
	}
	if pack.SchemaVersion != 1 || pack.Window != q || pack.Stats == nil || pack.Stats.TotalRuns != 3 {
		t.Errorf("pack header = version %d, window %+v, stats %+v", pack.SchemaVersion, pack.Window, pack.Stats)
	}
	if len(pack.Routines) != 1 {
		t.Fatalf("pack routines = %d, want 1", len(pack.Routines))
	}
	rt := pack.Routines[0]
	if rt.Name != "inventory" || rt.Generation != 1 || rt.Prompt != "list files" || rt.Schedule != "0 3 * * *" || fmt.Sprint(rt.AllowedTools) != "[Bash]" {
		t.Errorf("pack routine = %+v", rt)
	}
	// Problems newest first: the retained failure, then the cancelled attempt;
	// the verified success is excluded. Smoke 19 needs both kinds present.
	if len(pack.ProblemAttempts) != 2 {
		t.Fatalf("problem attempts = %d, want 2", len(pack.ProblemAttempts))
	}
	first, second := pack.ProblemAttempts[0], pack.ProblemAttempts[1]
	if first.Facts.AttemptID != aFail.ID || !first.Facts.Retained {
		t.Errorf("first problem = %s (retained=%v), want the retained failure %s", first.Facts.AttemptID, first.Facts.Retained, aFail.ID)
	}
	if second.Facts.AttemptID != aCancel.ID || second.Facts.State != model.Cancelled {
		t.Errorf("second problem = %s (%s), want the cancelled attempt", second.Facts.AttemptID, second.Facts.State)
	}
	if len(first.ResultText) != 2000 {
		t.Errorf("result text = %d bytes, want truncated to 2000", len(first.ResultText))
	}
	if !strings.Contains(string(first.Result), "failed") {
		t.Errorf("structured result = %s", first.Result)
	}
	if len(first.Events) != 30 {
		t.Fatalf("events = %d, want the last 30", len(first.Events))
	}
	if first.Events[0].Seq != 5 || first.Events[29].Seq != 34 {
		t.Errorf("event tail = seq %d..%d, want 5..34 (the LAST 30, not the first)", first.Events[0].Seq, first.Events[29].Seq)
	}
	for _, e := range first.Events {
		if e.Kind == protocol.KindStdout || e.Kind == protocol.KindStderr {
			t.Errorf("line event %d leaked into the pack", e.Seq)
		}
	}
	if second.Events == nil {
		t.Error("cancelled attempt events = nil, want an empty slice")
	}
}

// TestLoadFunnel pins the report's proposal funnel and cost-per-applied: the
// funnel is all-time (proposals are decided once, not windowed), and the cost
// numerator is only the window's retro-mode spend.
func TestLoadFunnel(t *testing.T) {
	f := newFixture(t)
	aRetro := f.attempt("retro1")
	aRun := f.attempt("run1")
	now := f.now
	f.write(func(tx *store.Tx) error {
		retro := f.facts(aRetro, model.Succeeded, now.Add(-time.Hour))
		retro.Mode, retro.CostUSD = "retro", fptr(3.0)
		if err := tx.InsertFacts(ctx(), retro); err != nil {
			return err
		}
		run := f.facts(aRun, model.Succeeded, now.Add(-time.Hour))
		run.CostUSD = fptr(10.0) // mode "run": never in the retro numerator
		return tx.InsertFacts(ctx(), run)
	})
	q := stats.Query{Since: now.Add(-24 * time.Hour), Until: now.Add(time.Minute)}

	// No proposals yet: the funnel is present and empty, the ratio undefined.
	r, err := stats.Load(ctx(), f.s, q)
	if err != nil {
		t.Fatal(err)
	}
	if r.Funnel == nil || *r.Funnel != (store.ProposalFunnel{}) {
		t.Fatalf("empty funnel = %+v", r.Funnel)
	}
	if r.CostPerApplied != nil {
		t.Errorf("cost per applied with nothing applied = %v, want nil", *r.CostPerApplied)
	}

	// One applied proposal, one still proposed.
	f.write(func(tx *store.Tx) error {
		p := &store.Proposal{Source: "manual", Kind: model.ProposalProcess, Target: "routine:inventory", Rationale: "r", VerificationPlan: "v"}
		if err := tx.CreateProposal(ctx(), p); err != nil {
			return err
		}
		if _, err := tx.DecideProposal(ctx(), p.ID, model.ProposalApproved, "human"); err != nil {
			return err
		}
		if _, err := tx.MarkProposalApplied(ctx(), p.ID, "generation:2"); err != nil {
			return err
		}
		return tx.CreateProposal(ctx(), &store.Proposal{Source: "manual", Kind: model.ProposalDoc, Target: "kb:x", Rationale: "r", VerificationPlan: "v"})
	})
	r, err = stats.Load(ctx(), f.s, q)
	if err != nil {
		t.Fatal(err)
	}
	want := store.ProposalFunnel{Proposed: 2, Approved: 1, Applied: 1}
	if r.Funnel == nil || *r.Funnel != want {
		t.Fatalf("funnel = %+v, want %+v", r.Funnel, want)
	}
	if r.CostPerApplied == nil {
		t.Fatal("cost per applied = nil, want 3.0")
	}
	approx(t, "cost per applied", *r.CostPerApplied, 3.0)
}
