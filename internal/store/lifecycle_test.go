package store

import (
	"context"
	"errors"
	"fmt"
	"testing"
	"time"

	"forge/internal/model"
	"forge/internal/protocol"
)

// fixture builds a store with a default project, one worker with one
// repository, and one routine, and returns a helper to create Work.
type fixture struct {
	t   testing.TB
	s   *Store
	now time.Time
}

func newFixture(t testing.TB) *fixture {
	t.Helper()
	f := &fixture{t: t, now: time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)}
	path := t.TempDir() + "/forge.sqlite3"
	s, err := Open(context.Background(), path, Options{Clock: func() time.Time { return f.now }})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := s.Close(); err != nil {
			t.Error(err)
		}
	})
	f.s = s
	f.write(func(tx *Tx) error {
		if err := tx.EnsureProject(ctx(), "default"); err != nil {
			return err
		}
		if err := tx.Register(ctx(), protocol.RegisterRequest{WorkerID: workerID, Name: "laptop", Version: "test", MaxConcurrent: 2, Executors: []string{"claude-code"}, Capabilities: map[string]string{"sandbox": "missing"},
			Repositories: []protocol.Repository{{Name: "equitizr", Path: "/tmp/equitizr", OriginIdentity: "github.com/x/equitizr"}}}); err != nil {
			return err
		}
		return tx.CreateRoutine(ctx(), &Routine{Name: "inventory", Mode: "run", Prompt: "list files in {{repo}}", Repositories: []string{"equitizr"}, Model: "haiku", TimeoutSeconds: 300})
	})
	return f
}

const workerID = "0123456789abcdef0123456789abcdef"

func ctx() context.Context { return context.Background() }

// must returns v or panics on err, so reads in tests never discard an error
// (STYLE.md §3). A panic in a test is a failure with a stack trace, which is
// what an unexpected store error deserves.
func must[T any](v T, err error) T {
	if err != nil {
		panic(fmt.Sprintf("unexpected error: %v", err))
	}
	return v
}

func (f *fixture) write(fn func(tx *Tx) error) {
	f.t.Helper()
	if err := f.s.Write(ctx(), fn); err != nil {
		f.t.Fatal(err)
	}
}

func (f *fixture) newWork(class model.BudgetClass, edges ...model.Edge) (*Work, Target) {
	f.t.Helper()
	w := &Work{RoutineName: "inventory", Generation: 1, Title: "inventory", Trigger: model.TriggerManual, Snapshot: []byte(`{}`), Priority: 100, BudgetClass: class, Autonomy: model.AutonomyAuto}
	var targets []Target
	f.write(func(tx *Tx) error {
		r, err := tx.GetRoutine(ctx(), "inventory")
		if err != nil {
			return err
		}
		w.RoutineID = r.ID
		targets, err = tx.CreateWork(ctx(), w, []string{"equitizr"}, edges)
		return err
	})
	return w, targets[0]
}

// run walks an attempt through the heartbeats a worker always sends before the
// agent starts: preparing, then running.
func (f *fixture) run(attemptID, lease string) {
	f.t.Helper()
	for _, st := range []model.State{model.Preparing, model.Running} {
		f.write(func(tx *Tx) error {
			_, err := tx.RecordHeartbeat(ctx(), attemptID, protocol.HeartbeatRequest{LeaseToken: lease, State: st})
			return err
		})
	}
}

func (f *fixture) claim(target Target, req string) *Attempt {
	f.t.Helper()
	var a *Attempt
	f.write(func(tx *Tx) error {
		var err error
		a, err = tx.Claim(ctx(), ClaimParams{TargetID: target.ID, WorkerID: workerID, ClaimRequestID: req, LeaseToken: "lease-" + req, MCPToken: "mcp-" + req, Executor: "claude-code", Model: "claude-haiku-4-5", ModelAlias: "haiku", Mode: "run", Autonomy: model.AutonomyAuto})
		return err
	})
	return a
}

func TestRoutineGenerationsAndConflicts(t *testing.T) {
	f := newFixture(t)
	r, err := f.s.GetRoutine(ctx(), "inventory")
	if err != nil || r.Generation != 1 || r.Executor != "claude-code" || r.Concurrency != 1 || r.MaxQuestions != 3 {
		t.Fatalf("routine = %+v, %v", r, err)
	}
	r.Prompt = "changed"
	f.write(func(tx *Tx) error { return tx.UpdateRoutine(ctx(), r, 1) })
	err = f.s.Write(ctx(), func(tx *Tx) error { return tx.UpdateRoutine(ctx(), r, 1) })
	if !errors.Is(err, ErrStaleGeneration) {
		t.Errorf("stale update: %v", err)
	}
	r2 := must(f.s.GetRoutine(ctx(), "inventory"))
	if r2.Generation != 2 || r2.Prompt != "changed" {
		t.Errorf("after update: %+v", r2)
	}
	err = f.s.Write(ctx(), func(tx *Tx) error {
		return tx.CreateRoutine(ctx(), &Routine{Name: "inventory", Mode: "run", Prompt: "x", Model: "haiku", TimeoutSeconds: 1})
	})
	if !errors.Is(err, ErrConflict) {
		t.Errorf("duplicate name: %v", err)
	}
	err = f.s.Write(ctx(), func(tx *Tx) error {
		return tx.CreateRoutine(ctx(), &Routine{Name: "Bad Name", Mode: "run", Prompt: "x", Model: "haiku", TimeoutSeconds: 1})
	})
	if err == nil {
		t.Error("bad name accepted")
	}
	var gens int
	if err := f.s.queryRow(ctx(), `SELECT count(*) FROM routine_generations`).Scan(&gens); err != nil || gens != 2 {
		t.Errorf("generations = %d, %v", gens, err)
	}
}

func TestClaimHeartbeatCompleteSucceeds(t *testing.T) {
	f := newFixture(t)
	w, target := f.newWork(model.ClassInteractive)
	f.now = f.now.Add(5 * time.Second)
	a := f.claim(target, "req-1")
	again := f.claim(target, "req-1") // lost response: same attempt back
	if again.ID != a.ID {
		t.Fatalf("claim retry created a second attempt")
	}
	tg := must(f.s.GetTarget(ctx(), target.ID))
	if tg.State != model.Claimed || tg.WorkerID != workerID || tg.LeaseExpiresAt.Sub(f.now) != LeaseDuration {
		t.Fatalf("after claim: %+v", tg)
	}
	f.write(func(tx *Tx) error {
		_, err := tx.RecordHeartbeat(ctx(), a.ID, protocol.HeartbeatRequest{LeaseToken: "lease-req-1", State: model.Preparing, Phase: "fetch", Worktree: "/wt", Branch: "forge/inventory-" + a.ID[:8], BaseBranch: "master", BaseCommit: "abc"})
		return err
	})
	f.now = f.now.Add(10 * time.Second)
	f.write(func(tx *Tx) error {
		_, err := tx.RecordHeartbeat(ctx(), a.ID, protocol.HeartbeatRequest{LeaseToken: "lease-req-1", State: model.Running, Phase: "agent", PID: 4242, PIDStart: 99, SessionID: "sess", PromptVersion: &protocol.PromptVersion{Hash: "h1", Routine: "inventory", Generation: 1, Mode: "run", Model: "haiku"}})
		return err
	})
	err := f.s.Write(ctx(), func(tx *Tx) error {
		_, _, err := tx.Heartbeat(ctx(), target.ID, "wrong-token")
		return err
	})
	if !errors.Is(err, ErrLease) {
		t.Errorf("wrong token: %v", err)
	}
	f.now = f.now.Add(20 * time.Second)
	var out *CompleteOutcome
	f.write(func(tx *Tx) error {
		var err error
		out, err = tx.Complete(ctx(), a.ID, protocol.CompleteRequest{LeaseToken: "lease-req-1", State: model.Succeeded, ExitCode: 0, NumTurns: 3, Usage: protocol.Usage{InputTokens: 10, OutputTokens: 20}, SessionID: "sess", Launches: 1,
			Git: protocol.GitOutcome{Head: "abc", Pushed: true}, Verification: protocol.Verification{Level: 1, Passed: true}, Cleanup: protocol.Cleanup{Outcome: "removed", Reason: "removed"}, StartedAt: f.now.Add(-30 * time.Second), FinishedAt: f.now}, 1)
		return err
	})
	if out.Late || out.Again || out.Target.State != model.Succeeded {
		t.Fatalf("outcome = %+v target %s", out, out.Target.State)
	}
	got := must(f.s.GetAttempt(ctx(), a.ID))
	if got.Launches != 1 || got.PID != 4242 || got.SessionID != "sess" || got.PromptVersionHash != "h1" || got.Usage.OutputTokens != 20 || got.VerificationLevel == nil || *got.VerificationLevel != 1 || got.Cleanup.Outcome != "removed" || got.StartedAt.IsZero() {
		t.Errorf("attempt = %+v", got)
	}
	wk := must(f.s.GetWork(ctx(), w.ID))
	if wk.FinishedAt.IsZero() {
		t.Error("work not finished")
	}
	// Idempotent retry of complete.
	f.write(func(tx *Tx) error {
		var err error
		out, err = tx.Complete(ctx(), a.ID, protocol.CompleteRequest{LeaseToken: "lease-req-1", State: model.Succeeded}, 1)
		return err
	})
	if !out.Again {
		t.Error("retry not idempotent")
	}
	hist := must(f.s.JournalForEntity(ctx(), EntityTarget, target.ID))
	kinds := []string{}
	for _, h := range hist {
		kinds = append(kinds, h.Kind)
	}
	if len(hist) != 5 { // claimed, preparing, running, verifying, succeeded
		t.Errorf("target journal = %v", kinds)
	}
	pv, err := f.s.GetPromptVersion(ctx(), "h1")
	if err != nil || pv.Routine != "inventory" {
		t.Errorf("prompt version: %+v %v", pv, err)
	}
}

func TestCompleteUnverifiedAndVerifyingWait(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassNormal)
	a := f.claim(target, "r1")
	f.run(a.ID, "lease-r1")
	f.write(func(tx *Tx) error {
		_, err := tx.Complete(ctx(), a.ID, protocol.CompleteRequest{LeaseToken: "lease-r1", State: model.Succeeded, Verification: protocol.Verification{Level: 1, Passed: false, Reason: "check_failed:test"}, Cleanup: protocol.Cleanup{Outcome: "retained", Reason: "dirty worktree"}, FinishedAt: f.now}, 1)
		return err
	})
	tg := must(f.s.GetTarget(ctx(), target.ID))
	if tg.State != model.Unverified || tg.UnverifiedReason != "check_failed:test" || !tg.Retained {
		t.Errorf("unverified target = %+v", tg)
	}
	_, target2 := f.newWork(model.ClassNormal)
	a2 := f.claim(target2, "r2")
	f.run(a2.ID, "lease-r2")
	f.write(func(tx *Tx) error {
		_, err := tx.Complete(ctx(), a2.ID, protocol.CompleteRequest{LeaseToken: "lease-r2", State: model.Succeeded, Verification: protocol.Verification{Level: 1, Passed: true}, FinishedAt: f.now}, 2)
		return err
	})
	tg2 := must(f.s.GetTarget(ctx(), target2.ID))
	if tg2.State != model.Verifying || !tg2.LeaseExpiresAt.IsZero() {
		t.Errorf("L2 target should wait in verifying without a lease: %+v", tg2)
	}
}

func TestSweepExpiredLeasesAndLateCompletion(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassNormal)
	a := f.claim(target, "r1")
	f.now = f.now.Add(LeaseDuration + time.Second)
	var swept []Target
	f.write(func(tx *Tx) error {
		var err error
		swept, err = tx.SweepExpiredLeases(ctx())
		return err
	})
	if len(swept) != 1 || swept[0].State != model.Failed || swept[0].FailureReason != model.ReasonLeaseExpired {
		t.Fatalf("swept = %+v", swept)
	}
	got := must(f.s.GetAttempt(ctx(), a.ID))
	if got.FinishedAt.IsZero() || got.FailureReason != model.ReasonLeaseExpired {
		t.Errorf("attempt after sweep = %+v", got)
	}
	// The worker finishes anyway: only cleanup fields land, nothing is lost.
	f.write(func(tx *Tx) error {
		return tx.PatchCleanup(ctx(), a.ID, protocol.CleanupPatch{Git: &protocol.GitOutcome{Head: "def", Commits: 1}, Cleanup: protocol.Cleanup{Outcome: "retained", Reason: "unpushed commits", Command: "forge cleanup x --confirm"}})
	})
	got = must(f.s.GetAttempt(ctx(), a.ID))
	tg := must(f.s.GetTarget(ctx(), target.ID))
	if got.Cleanup.Outcome != "retained" || got.Git.Commits != 1 || !tg.Retained {
		t.Errorf("after patch: %+v %+v", got.Cleanup, tg)
	}
}

func TestExtendLeasesOnRestart(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassNormal)
	f.claim(target, "r1")
	f.now = f.now.Add(10 * time.Minute) // the daemon was down far longer than the lease
	var n int
	f.write(func(tx *Tx) error {
		var err error
		n, err = tx.ExtendLeases(ctx())
		if err != nil {
			return err
		}
		swept, err := tx.SweepExpiredLeases(ctx())
		if len(swept) != 0 {
			t.Errorf("sweeper expired a just-extended lease: %v", swept)
		}
		return err
	})
	tg := must(f.s.GetTarget(ctx(), target.ID))
	if n != 1 || tg.State != model.Claimed || tg.LeaseExpiresAt.Sub(f.now) != RestartGrace {
		t.Errorf("extend: n=%d target=%+v", n, tg)
	}
}

func TestCancelPendingAndLeased(t *testing.T) {
	f := newFixture(t)
	w1, t1 := f.newWork(model.ClassNormal)
	w2, t2 := f.newWork(model.ClassNormal)
	f.claim(t2, "r2")
	f.write(func(tx *Tx) error {
		if err := tx.CancelWork(ctx(), w1.ID, "human"); err != nil {
			return err
		}
		return tx.CancelWork(ctx(), w2.ID, "human")
	})
	g1 := must(f.s.GetTarget(ctx(), t1.ID))
	g2 := must(f.s.GetTarget(ctx(), t2.ID))
	if g1.State != model.Cancelled || g2.State != model.Claimed || !g2.CancelRequested {
		t.Errorf("cancel: %+v %+v", g1, g2)
	}
	var cancel bool
	f.write(func(tx *Tx) error {
		var err error
		cancel, _, err = tx.Heartbeat(ctx(), t2.ID, "lease-r2")
		return err
	})
	if !cancel {
		t.Error("heartbeat did not report cancellation")
	}
}

func TestQuestionPausesAndAnswerRequeues(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassNormal)
	a := f.claim(target, "r1")
	f.run(a.ID, "lease-r1")
	f.write(func(tx *Tx) error {
		_, err := tx.Complete(ctx(), a.ID, protocol.CompleteRequest{LeaseToken: "lease-r1", State: model.WaitingHuman, SessionID: "sess-1", Launches: 1, Question: &protocol.QuestionRequest{Text: "Which README?", Options: []string{"a", "b"}}, FinishedAt: f.now}, 1)
		return err
	})
	tg := must(f.s.GetTarget(ctx(), target.ID))
	if tg.State != model.WaitingHuman || !tg.LeaseExpiresAt.IsZero() {
		t.Fatalf("after question: %+v", tg)
	}
	open := must(f.s.OpenQuestions(ctx()))
	if len(open) != 1 || open[0].Text != "Which README?" {
		t.Fatalf("open questions = %+v", open)
	}
	f.now = f.now.Add(time.Hour)
	f.write(func(tx *Tx) error {
		_, err := tx.AnswerQuestion(ctx(), open[0].ID, "b", "human")
		return err
	})
	tg = must(f.s.GetTarget(ctx(), target.ID))
	if tg.State != model.Pending || tg.WorkerID != workerID {
		t.Fatalf("after answer: %+v", tg)
	}
	// Re-claim resumes the same attempt.
	a2 := f.claim(target, "r1-resume")
	if a2.ID != a.ID {
		t.Errorf("resume created a new attempt")
	}
	got := must(f.s.GetAttempt(ctx(), a.ID))
	if !got.FinishedAt.IsZero() || got.SessionID != "sess-1" {
		t.Errorf("resumed attempt = %+v", got)
	}
	var last *Question
	f.write(func(tx *Tx) error {
		var err error
		last, err = tx.LastAnswer(ctx(), a.ID)
		return err
	})
	if last == nil || last.Answer != "b" {
		t.Errorf("last answer = %+v", last)
	}
	err := f.s.Write(ctx(), func(tx *Tx) error {
		_, err := tx.AnswerQuestion(ctx(), open[0].ID, "again", "human")
		return err
	})
	if !errors.Is(err, ErrConflict) {
		t.Errorf("double answer: %v", err)
	}
}

// TestAnswerDoesNotResurrectCancelledTarget guards the state-machine bug where
// answering a stale question whose target was cancelled while the question sat
// open transitioned the (terminal) target back to pending — cancelled → pending
// is a legal edge (it exists for retry) — leaving a pending Target on a finished
// Work that the scheduler never re-claims: an inert zombie.
func TestAnswerDoesNotResurrectCancelledTarget(t *testing.T) {
	f := newFixture(t)
	work, target := f.newWork(model.ClassNormal)
	a := f.claim(target, "r1")
	f.run(a.ID, "lease-r1")
	f.write(func(tx *Tx) error {
		_, err := tx.Complete(ctx(), a.ID, protocol.CompleteRequest{LeaseToken: "lease-r1", State: model.WaitingHuman, SessionID: "sess-1", Launches: 1, Question: &protocol.QuestionRequest{Text: "Which one?"}, FinishedAt: f.now}, 1)
		return err
	})
	// Cancel the target while it waits on the question; the Work finishes.
	f.write(func(tx *Tx) error {
		_, err := tx.Transition(ctx(), target.ID, model.Cancelled, TransitionOptions{Reason: model.ReasonCancelled, Actor: "human"})
		return err
	})
	wk := must(f.s.GetWork(ctx(), work.ID))
	if wk.FinishedAt.IsZero() {
		t.Fatalf("work not finished after cancel: %+v", wk)
	}

	// Answer the still-open question long after the cancel.
	f.now = f.now.Add(12 * time.Hour)
	open := must(f.s.OpenQuestions(ctx()))
	if len(open) != 1 {
		t.Fatalf("open questions = %+v", open)
	}
	f.write(func(tx *Tx) error {
		_, err := tx.AnswerQuestion(ctx(), open[0].ID, "the answer", "human")
		return err
	})

	// The target must stay cancelled — not resurrected to pending — and its
	// Work must stay finished. The answer is still recorded for the record.
	tg := must(f.s.GetTarget(ctx(), target.ID))
	if tg.State != model.Cancelled {
		t.Errorf("target resurrected: state = %s, want cancelled", tg.State)
	}
	wk = must(f.s.GetWork(ctx(), work.ID))
	if wk.FinishedAt.IsZero() {
		t.Errorf("work reopened by stale answer: %+v", wk)
	}
	if q := must(f.s.OpenQuestions(ctx())); len(q) != 0 {
		t.Errorf("question not recorded as answered: %+v", q)
	}
}

func TestDependenciesAndCycles(t *testing.T) {
	f := newFixture(t)
	a, _ := f.newWork(model.ClassBacklog)
	b, _ := f.newWork(model.ClassNormal, model.Edge{BlockedBy: a.ID, On: model.OnSuccess})
	err := f.s.Write(ctx(), func(tx *Tx) error {
		return tx.AddDependency(ctx(), model.Edge{Work: a.ID, BlockedBy: b.ID, On: model.OnTerminal})
	})
	if !errors.Is(err, ErrConflict) {
		t.Errorf("cycle accepted: %v", err)
	}
	edges := must(f.s.DependencyEdges(ctx()))
	if len(edges) != 1 || edges[0].Work != b.ID || edges[0].BlockedBy != a.ID {
		t.Errorf("edges = %+v", edges)
	}
	f.write(func(tx *Tx) error { return tx.SetPriority(ctx(), a.ID, 7) })
	f.write(func(tx *Tx) error { return tx.RemoveDependency(ctx(), b.ID, a.ID) })
	edges = must(f.s.DependencyEdges(ctx()))
	if len(edges) != 0 {
		t.Errorf("edges after remove = %+v", edges)
	}
	got := must(f.s.GetWork(ctx(), a.ID))
	if got.Priority != 7 {
		t.Errorf("priority = %d", got.Priority)
	}
}

func TestEventsBatchDedupeAndFacts(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassNormal)
	a := f.claim(target, "r1")
	evs := []protocol.Event{
		{Seq: 0, Time: f.now, ElapsedUS: 0, Kind: protocol.KindLifecycle, Message: "claimed"},
		{Seq: 1, Time: f.now, ElapsedUS: 10, Kind: protocol.KindSpanStart, SpanID: "fetch", Name: "fetch"},
		{Seq: 2, Time: f.now, ElapsedUS: 500, Kind: protocol.KindSpanEnd, SpanID: "fetch", Name: "fetch", DurationUS: 490},
		{Seq: 3, Time: f.now, ElapsedUS: 600, Kind: protocol.KindStdout, Message: "line"},
	}
	var n int
	f.write(func(tx *Tx) error {
		var err error
		n, err = tx.InsertEvents(ctx(), a.ID, protocol.SourceWorker, evs)
		return err
	})
	f.write(func(tx *Tx) error {
		m, err := tx.InsertEvents(ctx(), a.ID, protocol.SourceWorker, evs[1:]) // redelivery
		if m != 0 {
			t.Errorf("redelivered %d events", m)
		}
		return err
	})
	if n != 4 {
		t.Fatalf("inserted %d", n)
	}
	err := f.s.Write(ctx(), func(tx *Tx) error {
		_, err := tx.InsertEvents(ctx(), a.ID, protocol.SourceWorker, []protocol.Event{{Seq: 9, Kind: "bogus"}})
		return err
	})
	if err == nil {
		t.Error("invalid event accepted")
	}
	spans := must(f.s.Events(ctx(), a.ID, false, 0))
	all := must(f.s.Events(ctx(), a.ID, true, 0))
	if len(spans) != 3 || len(all) != 4 || spans[2].DurationUS != 490 {
		t.Errorf("events: %d spans, %d all", len(spans), len(all))
	}
	count, bytes, err := f.s.LineEventStats(ctx(), a.ID)
	if err != nil {
		t.Fatal(err)
	}
	if count != 1 || bytes != 4 {
		t.Errorf("line stats = %d %d", count, bytes)
	}
	total := int64(1234)
	facts := &AttemptFacts{AttemptID: a.ID, TargetID: target.ID, WorkID: target.WorkID, Routine: "inventory", Generation: 1, Project: "default", Repository: "equitizr", Worker: workerID, Executor: "claude-code", Model: "haiku", Mode: "run", Trigger: model.TriggerManual, Autonomy: model.AutonomyAuto,
		Phases: map[string]*int64{"total": &total, "fetch": &total}, FinishedAt: f.now, State: model.Succeeded, ToolCallsByName: map[string]int{"Bash": 2}, ToolTimeByName: map[string]int64{"Bash": 50}}
	f.write(func(tx *Tx) error { return tx.InsertFacts(ctx(), facts) })
	err = f.s.Write(ctx(), func(tx *Tx) error { return tx.InsertFacts(ctx(), facts) })
	if !errors.Is(err, ErrConflict) {
		t.Errorf("facts written twice: %v", err)
	}
	got, err := f.s.FactsForAttempt(ctx(), a.ID)
	if err != nil || *got.Phases["total"] != 1234 || got.Phases["agent"] != nil || got.ToolCallsByName["Bash"] != 2 || got.Turns != nil {
		t.Errorf("facts = %+v, %v", got, err)
	}
	rows := must(f.s.FactsSince(ctx(), f.now.Add(-time.Hour), f.now.Add(time.Hour), "inventory"))
	if len(rows) != 1 {
		t.Errorf("FactsSince = %d", len(rows))
	}
}

func TestWorkersRepositoriesSamples(t *testing.T) {
	f := newFixture(t)
	ws := must(f.s.Workers(ctx(), f.now))
	if len(ws) != 1 || !ws[0].Connected || ws[0].Capabilities["sandbox"] != "missing" {
		t.Errorf("workers = %+v", ws)
	}
	ws = must(f.s.Workers(ctx(), f.now.Add(2*time.Minute)))
	if ws[0].Connected {
		t.Error("stale worker reported connected")
	}
	repos := must(f.s.Repositories(ctx()))
	if len(repos) != 1 || repos[0].Project != "default" || repos[0].WorkerID != workerID {
		t.Errorf("repositories = %+v", repos)
	}
	f.write(func(tx *Tx) error {
		return tx.InsertSamples(ctx(), []RateLimitSample{{Time: f.now, Window: "five_hour", Utilization: 0.4, ResetsAt: f.now.Add(time.Hour)}, {Time: f.now, Window: "five_hour", Utilization: 0.4, ResetsAt: f.now.Add(time.Hour)}})
	})
	latest := must(f.s.LatestSample(ctx(), "five_hour"))
	since := must(f.s.SamplesSince(ctx(), "five_hour", f.now.Add(-time.Minute)))
	if latest == nil || latest.Utilization != 0.4 || len(since) != 1 {
		t.Errorf("samples: %+v %d", latest, len(since))
	}
	if s := must(f.s.LatestSample(ctx(), "seven_day")); s != nil {
		t.Error("phantom sample")
	}
}
