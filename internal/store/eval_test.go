package store

import (
	"context"
	"errors"
	"testing"

	"forge/internal/model"
)

func TestSetProposalEvalScore(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	p := mkProposal(t, st, model.ProposalRoutine, "routine:inventory")

	var scored *Proposal
	err := st.Write(ctx, func(tx *Tx) error {
		var werr error
		scored, werr = tx.SetProposalEvalScore(ctx, p.ID, 0.75)
		return werr
	})
	if err != nil {
		t.Fatal(err)
	}
	if scored.EvalScore == nil || *scored.EvalScore != 0.75 {
		t.Errorf("returned eval score = %v", scored.EvalScore)
	}
	got, err := st.GetProposal(ctx, p.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.EvalScore == nil || *got.EvalScore != 0.75 {
		t.Errorf("stored eval score = %v", got.EvalScore)
	}
	entries, err := st.JournalForEntity(ctx, EntityProposal, p.ID)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, e := range entries {
		if e.Kind == "proposal.eval_scored" {
			found = true
		}
	}
	if !found {
		t.Errorf("no proposal.eval_scored journal row in %d entries", len(entries))
	}
}

func TestSetProposalEvalScoreRefusals(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	p := mkProposal(t, st, model.ProposalRoutine, "routine:inventory")

	// Out of range.
	err := st.Write(ctx, func(tx *Tx) error {
		_, werr := tx.SetProposalEvalScore(ctx, p.ID, 1.5)
		return werr
	})
	if err == nil {
		t.Error("score 1.5 accepted")
	}

	// Unknown id.
	err = st.Write(ctx, func(tx *Tx) error {
		_, werr := tx.SetProposalEvalScore(ctx, "ffffffffffffffffffffffffffffffff", 0.5)
		return werr
	})
	if !errors.Is(err, ErrNotFound) {
		t.Errorf("unknown id = %v, want ErrNotFound", err)
	}

	// Decided proposals no longer take a score.
	if err := st.Write(ctx, func(tx *Tx) error {
		_, werr := tx.DecideProposal(ctx, p.ID, model.ProposalRejected, "human")
		return werr
	}); err != nil {
		t.Fatal(err)
	}
	err = st.Write(ctx, func(tx *Tx) error {
		_, werr := tx.SetProposalEvalScore(ctx, p.ID, 0.5)
		return werr
	})
	if !errors.Is(err, ErrConflict) {
		t.Errorf("score on rejected = %v, want ErrConflict", err)
	}
}
