package store

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"testing"

	"forge/internal/model"
)

func mkProposal(t *testing.T, st *Store, kind model.ProposalKind, target string) *Proposal {
	t.Helper()
	p := &Proposal{Source: "manual", Kind: kind, Target: target,
		After: json.RawMessage(`{"prompt":"new"}`), Rationale: "r", VerificationPlan: "v"}
	if err := st.Write(context.Background(), func(tx *Tx) error { return tx.CreateProposal(context.Background(), p) }); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestProposalLifecycle(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	p := mkProposal(t, st, model.ProposalRoutine, "routine:inventory")
	if p.Status != model.ProposalProposed || p.ID == "" {
		t.Fatalf("created = %+v", p)
	}

	// Reject an unrelated one; approve → apply → revert the main one.
	q := mkProposal(t, st, model.ProposalDoc, "kb:some-note")
	if err := st.Write(ctx, func(tx *Tx) error {
		if _, err := tx.DecideProposal(ctx, q.ID, model.ProposalRejected, "nate"); err != nil {
			return err
		}
		if _, err := tx.DecideProposal(ctx, p.ID, model.ProposalApproved, "nate"); err != nil {
			return err
		}
		if _, err := tx.MarkProposalApplied(ctx, p.ID, "generation:2"); err != nil {
			return err
		}
		_, err := tx.MarkProposalReverted(ctx, p.ID, json.RawMessage(`{"verified_rate_delta":-0.5}`))
		return err
	}); err != nil {
		t.Fatal(err)
	}

	got, err := st.GetProposal(ctx, p.ID[:8]) // prefix lookup
	if err != nil {
		t.Fatal(err)
	}
	if got.Status != model.ProposalReverted || got.AppliedRef != "generation:2" || got.DecidedBy != "nate" || got.DecidedAt.IsZero() {
		t.Errorf("after lifecycle = %+v", got)
	}
	if string(got.OutcomeMetrics) != `{"verified_rate_delta":-0.5}` {
		t.Errorf("outcome = %s", got.OutcomeMetrics)
	}

	// Illegal transitions are conflicts.
	err = st.Write(ctx, func(tx *Tx) error {
		_, err := tx.DecideProposal(ctx, p.ID, model.ProposalApproved, "nate")
		return err
	})
	if !errors.Is(err, ErrConflict) {
		t.Errorf("re-decide reverted = %v", err)
	}

	// Funnel: 2 proposed total; 1 reached approved+applied+reverted; 1 rejected.
	f, err := st.Funnel(ctx)
	if err != nil {
		t.Fatal(err)
	}
	want := ProposalFunnel{Proposed: 2, Approved: 1, Applied: 1, Reverted: 1, Rejected: 1}
	if f != want {
		t.Errorf("funnel = %+v, want %+v", f, want)
	}

	// Listing newest first, filtered.
	all, err := st.ListProposals(ctx, "")
	if err != nil || len(all) != 2 {
		t.Fatalf("list = %d, %v", len(all), err)
	}
	rej, err := st.ListProposals(ctx, model.ProposalRejected)
	if err != nil || len(rej) != 1 || rej[0].ID != q.ID {
		t.Fatalf("filtered = %+v, %v", rej, err)
	}
}

func TestProposalConstitutionGuard(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	for _, target := range []string{"docs/CONSTITUTION.md", "CONSTITUTION.md", "constitution"} {
		p := &Proposal{Source: "manual", Kind: model.ProposalDoc, Target: target, Rationale: "r", VerificationPlan: "v"}
		err := st.Write(ctx, func(tx *Tx) error { return tx.CreateProposal(ctx, p) })
		if err == nil || !strings.Contains(err.Error(), "constitution") {
			t.Errorf("target %q: err = %v", target, err)
		}
	}
	bad := &Proposal{Source: "manual", Kind: "nonsense", Target: "x", Rationale: "r", VerificationPlan: "v"}
	if err := st.Write(ctx, func(tx *Tx) error { return tx.CreateProposal(ctx, bad) }); err == nil {
		t.Error("invalid kind accepted")
	}
	missing := &Proposal{Source: "manual", Kind: model.ProposalDoc, Target: "x"}
	if err := st.Write(ctx, func(tx *Tx) error { return tx.CreateProposal(ctx, missing) }); err == nil {
		t.Error("missing rationale accepted")
	}
}

func TestUpdateRoutineFromRecordsSource(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	r := &Routine{Name: "abtest", Mode: "run", Prompt: "p", Repositories: []string{"forge"},
		Executor: "claude-code", Model: "haiku", TimeoutSeconds: 60, BudgetClass: model.ClassNormal, Concurrency: 1}
	if err := st.Write(ctx, func(tx *Tx) error { return tx.CreateRoutine(ctx, r) }); err != nil {
		t.Fatal(err)
	}
	r.Prompt = "p2"
	if err := st.Write(ctx, func(tx *Tx) error { return tx.UpdateRoutineFrom(ctx, r, 1, "proposal:abc123") }); err != nil {
		t.Fatal(err)
	}
	var source string
	if err := st.queryRow(ctx, `SELECT source FROM routine_generations WHERE routine_id = ? AND generation = 2`, r.ID).Scan(&source); err != nil {
		t.Fatal(err)
	}
	if source != "proposal:abc123" {
		t.Errorf("source = %q", source)
	}
}

// A change-applying proposal must carry a concrete `after` at creation, so an
// approval never dead-ends later on "after is required".
func TestProposalRequiresAfter(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	for _, kind := range []model.ProposalKind{model.ProposalRoutine, model.ProposalModePrompt, model.ProposalProcess} {
		p := &Proposal{Source: "retro", Kind: kind, Target: "routine:inventory", Rationale: "r", VerificationPlan: "v"}
		if err := st.Write(ctx, func(tx *Tx) error { return tx.CreateProposal(ctx, p) }); err == nil {
			t.Errorf("kind %s with no after was accepted", kind)
		}
	}
	// A doc proposal with no after is fine (apply just records it).
	doc := &Proposal{Source: "retro", Kind: model.ProposalDoc, Target: "kb:note", Rationale: "r", VerificationPlan: "v"}
	if err := st.Write(ctx, func(tx *Tx) error { return tx.CreateProposal(ctx, doc) }); err != nil {
		t.Errorf("doc proposal without after rejected: %v", err)
	}
	// A routine proposal WITH a concrete after is accepted.
	ok := &Proposal{Source: "retro", Kind: model.ProposalRoutine, Target: "routine:inventory",
		After: json.RawMessage(`{"max_turns":8}`), Rationale: "r", VerificationPlan: "v"}
	if err := st.Write(ctx, func(tx *Tx) error { return tx.CreateProposal(ctx, ok) }); err != nil {
		t.Errorf("routine proposal with after rejected: %v", err)
	}
}
