package store

import (
	"testing"

	"forge/internal/core/model"
	"forge/internal/protocol"
)

// A claim with globs takes the path lease; the lease survives the running
// states and is released the moment the Target leaves them (M9).
func TestPathLeaseLifecycle(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassNormal)
	a := f.claim(target, "lease-life")
	f.write(func(tx *Tx) error {
		// Re-claiming inserts the lease: Claim was called by f.claim without
		// globs, so lease it explicitly through the same seam.
		_, err := tx.Exec(ctx(), `DELETE FROM path_leases`)
		return err
	})
	_, t2 := f.newWork(model.ClassNormal)
	var a2 *Attempt
	f.write(func(tx *Tx) error {
		var err error
		a2, err = tx.Claim(ctx(), ClaimParams{TargetID: t2.ID, WorkerID: workerID, ClaimRequestID: "lease-r2", LeaseToken: "lease-lease-r2", MCPToken: "m", Executor: "claude-code", Model: "m", ModelAlias: "haiku", Mode: "run", Autonomy: model.AutonomyAuto, Globs: []string{"docs/**"}})
		return err
	})
	_ = a
	var leases []PathLease
	f.write(func(tx *Tx) error {
		var err error
		leases, err = tx.PathLeases(ctx())
		return err
	})
	if len(leases) != 1 || leases[0].TargetID != t2.ID || len(leases[0].Globs) != 1 || leases[0].Globs[0] != "docs/**" {
		t.Fatalf("leases = %+v", leases)
	}
	// Through preparing and running the lease holds; on failure it is gone.
	f.run(a2.ID, "lease-lease-r2")
	if got := must(f.s.PathLeasesRead(ctx())); len(got) != 1 {
		t.Fatalf("lease must survive running: %+v", got)
	}
	f.write(func(tx *Tx) error {
		_, err := tx.Transition(ctx(), t2.ID, model.Failed, TransitionOptions{Reason: model.ReasonExitNonzero, Actor: workerID})
		return err
	})
	if got := must(f.s.PathLeasesRead(ctx())); len(got) != 0 {
		t.Fatalf("lease must be released at terminal: %+v", got)
	}
}

// MarkLeaseBlocked journals once; LeaseBlockedAt reads the first row back.
func TestMarkLeaseBlockedOnce(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassNormal)
	for range 3 {
		f.write(func(tx *Tx) error { return tx.MarkLeaseBlocked(ctx(), target.ID, "holder-1") })
	}
	rows := must(f.s.JournalForEntity(ctx(), EntityTarget, target.ID))
	n := 0
	for _, r := range rows {
		if r.Kind == "target.lease_blocked" {
			n++
		}
	}
	if n != 1 {
		t.Fatalf("lease_blocked journal rows = %d, want 1", n)
	}
	at := must(f.s.LeaseBlockedAt(ctx(), target.ID))
	if at.IsZero() {
		t.Fatal("LeaseBlockedAt must find the row")
	}
	other := must(f.s.LeaseBlockedAt(ctx(), "00000000000000000000000000000000"))
	if !other.IsZero() {
		t.Fatal("LeaseBlockedAt for an unblocked target must be zero")
	}
}

// A resumed claim re-takes the lease through the same Claim seam.
func TestPathLeaseOnResume(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassNormal)
	var a *Attempt
	f.write(func(tx *Tx) error {
		var err error
		a, err = tx.Claim(ctx(), ClaimParams{TargetID: target.ID, WorkerID: workerID, ClaimRequestID: "res-1", LeaseToken: "lease-res-1", MCPToken: "m", Executor: "claude-code", Model: "m", ModelAlias: "haiku", Mode: "run", Autonomy: model.AutonomyAsk, Globs: []string{"a/**"}})
		return err
	})
	f.run(a.ID, "lease-res-1")
	f.write(func(tx *Tx) error {
		_, err := tx.Complete(ctx(), a.ID, protocol.CompleteRequest{LeaseToken: "lease-res-1", State: model.WaitingHuman, Question: &protocol.QuestionRequest{Text: "which?"}, FinishedAt: f.now}, 1)
		return err
	})
	if got := must(f.s.PathLeasesRead(ctx())); len(got) != 0 {
		t.Fatalf("waiting_human must not hold the lease: %+v", got)
	}
	qs := must(f.s.QuestionsForWork(ctx(), target.WorkID))
	f.write(func(tx *Tx) error {
		_, err := tx.AnswerQuestion(ctx(), qs[0].ID, "that one", "human")
		return err
	})
	f.write(func(tx *Tx) error {
		_, err := tx.Claim(ctx(), ClaimParams{TargetID: target.ID, WorkerID: workerID, ClaimRequestID: "res-2", LeaseToken: "lease-res-2", MCPToken: "m", Executor: "claude-code", Model: "m", ModelAlias: "haiku", Mode: "run", Autonomy: model.AutonomyAsk, Globs: []string{"a/**"}})
		return err
	})
	if got := must(f.s.PathLeasesRead(ctx())); len(got) != 1 {
		t.Fatalf("resume must re-take the lease: %+v", got)
	}
}
