package store

import (
	"context"
	"fmt"

	"forge/internal/core/model"
)

// SetProposalEvalScore records the summary score `forge eval` measured for a
// proposal (the pass fraction of its golden cases). Approval of routine and
// mode_prompt proposals requires it (DESIGN.md §23); recording is allowed
// only while the proposal is still undecided — a score attached after the
// decision could not have informed it.
func (tx *Tx) SetProposalEvalScore(ctx context.Context, id string, score float64) (*Proposal, error) {
	if score < 0 || score > 1 {
		return nil, fmt.Errorf("eval score %v: want 0..1", score)
	}
	p, err := tx.getProposal(ctx, id)
	if err != nil {
		return nil, err
	}
	if p.Status != model.ProposalProposed {
		return nil, fmt.Errorf("proposal %s is %s; eval scores attach before the decision: %w", id, p.Status, ErrConflict)
	}
	if _, err := tx.Exec(ctx, `UPDATE proposals SET eval_score = ?, updated_at = ? WHERE id = ?`,
		score, formatTime(tx.now), id); err != nil {
		return nil, fmt.Errorf("set eval score on proposal %s: %w", id, err)
	}
	p.EvalScore, p.UpdatedAt = &score, tx.now
	if err := tx.Journal(ctx, "proposal.eval_scored", EntityProposal, id, map[string]any{"score": score}); err != nil {
		return nil, err
	}
	return p, nil
}
