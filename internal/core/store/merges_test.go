package store

import (
	"errors"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
)

// integratePastVerify walks an integrating Target to queued_for_merge the way
// the daemon does: claim, run, verifying, succeeded, queued.
func integratePastVerify(f *fixture) (*Work, Target, *Attempt) {
	f.t.Helper()
	w := &Work{RoutineName: "inventory", Generation: 1, Title: "integrating", Trigger: model.TriggerManual, Snapshot: []byte(`{}`), Priority: 100, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto, Integrate: true}
	var targets []Target
	f.write(func(tx *Tx) error {
		var err error
		targets, err = tx.CreateWork(ctx(), w, []string{"equitizr"}, nil)
		return err
	})
	target := targets[0]
	a := f.claim(target, "merge-1")
	f.run(a.ID, "lease-merge-1")
	f.write(func(tx *Tx) error {
		_, err := tx.Complete(ctx(), a.ID, protocol.CompleteRequest{LeaseToken: "lease-merge-1", State: model.Succeeded, Verification: protocol.Verification{Level: 1, Passed: true}, FinishedAt: f.now}, 1)
		return err
	})
	f.write(func(tx *Tx) error {
		_, err := tx.Transition(ctx(), target.ID, model.QueuedForMerge, TransitionOptions{Actor: "daemon"})
		return err
	})
	return w, target, a
}

func TestMergeRowAndJournal(t *testing.T) {
	f := newFixture(t)
	_, target, _ := integratePastVerify(f)
	var m *Merge
	f.write(func(tx *Tx) error {
		var err error
		m, err = tx.BeginMerge(ctx(), target.ID, "equitizr", "main")
		return err
	})
	if m.RebaseAttempts != 1 || m.IntegrationBranch != "main" {
		t.Fatalf("merge = %+v", m)
	}
	// A second pass increments in place.
	f.write(func(tx *Tx) error {
		var err error
		m, err = tx.BeginMerge(ctx(), target.ID, "equitizr", "main")
		return err
	})
	if m.RebaseAttempts != 2 {
		t.Fatalf("rebase_attempts = %d, want 2", m.RebaseAttempts)
	}
	f.write(func(tx *Tx) error {
		return tx.FinishMerge(ctx(), m.ID, "merged", "aaa111", "bbb222", map[string]any{"target_id": target.ID})
	})
	row := must(f.s.MergeForTarget(ctx(), target.ID))
	if row == nil || row.Outcome != "merged" || row.BeforeSHA != "aaa111" || row.AfterSHA != "bbb222" || row.PushedAt.IsZero() {
		t.Fatalf("finished merge = %+v", row)
	}
	pushed := false
	for _, e := range must(f.s.JournalForEntity(ctx(), EntityMerge, m.ID)) {
		if e.Kind == "merge.pushed" {
			pushed = true
		}
	}
	if !pushed {
		t.Fatal("merge.pushed journal row missing")
	}
}

func TestTargetsInState(t *testing.T) {
	f := newFixture(t)
	_, target, _ := integratePastVerify(f)
	queued := must(f.s.TargetsInState(ctx(), model.QueuedForMerge))
	if len(queued) != 1 || queued[0].ID != target.ID {
		t.Fatalf("queued = %+v", queued)
	}
	if got := must(f.s.TargetsInState(ctx(), model.Merging)); len(got) != 0 {
		t.Fatalf("merging = %+v", got)
	}
}

// The integration columns fill exactly once, NULL → value (DESIGN.md §9.2).
func TestUpdateIntegrationFactsOnce(t *testing.T) {
	f := newFixture(t)
	w, target, a := integratePastVerify(f)
	f.write(func(tx *Tx) error {
		return tx.InsertFacts(ctx(), &AttemptFacts{AttemptID: a.ID, TargetID: target.ID, WorkID: w.ID, Routine: "inventory", Project: "default", Repository: "equitizr", Worker: workerID, Executor: "claude-code", Model: "haiku", Mode: "run", Trigger: model.TriggerManual, Autonomy: model.AutonomyAuto, State: model.Succeeded, FinishedAt: f.now})
	})
	wait, attempts, depth := int64(1500), 2, 1
	fill := IntegrationFacts{MergeWaitUS: &wait, RebaseAttempts: &attempts, MergeOutcome: "merged", StackDepth: &depth, TouchedPaths: []string{"a.go"}}
	f.write(func(tx *Tx) error { return tx.UpdateIntegrationFacts(ctx(), a.ID, fill) })
	got := must(f.s.FactsForAttempt(ctx(), a.ID))
	if got.MergeOutcome != "merged" || got.MergeWaitUS == nil || *got.MergeWaitUS != 1500 || got.RebaseAttempts == nil || *got.RebaseAttempts != 2 || got.StackDepth == nil || *got.StackDepth != 1 {
		t.Fatalf("facts = %+v", got)
	}
	if len(got.TouchedPaths) != 1 || got.TouchedPaths[0] != "a.go" {
		t.Fatalf("touched = %v", got.TouchedPaths)
	}
	err := f.s.Write(ctx(), func(tx *Tx) error { return tx.UpdateIntegrationFacts(ctx(), a.ID, fill) })
	if !errors.Is(err, ErrConflict) {
		t.Fatalf("second fill must conflict, got %v", err)
	}
	err = f.s.Write(ctx(), func(tx *Tx) error { return tx.UpdateIntegrationFacts(ctx(), "00000000000000000000000000000000", fill) })
	if !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing row must be ErrNotFound, got %v", err)
	}
}

func TestResetMergeAttempts(t *testing.T) {
	f := newFixture(t)
	_, target, _ := integratePastVerify(f)
	var m *Merge
	for range 3 {
		f.write(func(tx *Tx) error {
			var err error
			m, err = tx.BeginMerge(ctx(), target.ID, "equitizr", "main")
			return err
		})
	}
	if m.RebaseAttempts != 3 {
		t.Fatalf("rebase_attempts = %d, want 3", m.RebaseAttempts)
	}
	// A human requeue grants a fresh budget: the next BeginMerge counts from 1.
	f.write(func(tx *Tx) error { return tx.ResetMergeAttempts(ctx(), target.ID) })
	f.write(func(tx *Tx) error {
		var err error
		m, err = tx.BeginMerge(ctx(), target.ID, "equitizr", "main")
		return err
	})
	if m.RebaseAttempts != 1 {
		t.Fatalf("rebase_attempts after reset = %d, want 1", m.RebaseAttempts)
	}
}
