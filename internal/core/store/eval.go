package store

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	"forge/internal/core/model"
)

// EvalCase is one golden case's verdict from one eval run — the per-case
// history behind proposals.eval_score's single float, so a prompt change's
// effect on the fixtures is a time series.
type EvalCase struct {
	ID            string    `json:"id"`
	ProposalID    string    `json:"proposal_id,omitempty"` // "" for ad-hoc `forge eval` runs
	Mode          string    `json:"mode"`
	CaseName      string    `json:"case"`
	Pass          bool      `json:"pass"`
	State         string    `json:"state"`
	FailureReason string    `json:"failure_reason,omitempty"`
	Turns         *int      `json:"turns,omitempty"`
	CostUSD       *float64  `json:"cost_usd,omitempty"`
	Details       string    `json:"details,omitempty"`
	CreatedAt     time.Time `json:"created_at"`
}

// InsertEvalCases records one run's per-case verdicts (all stamped tx.now, so
// a run reads back as one group).
func (tx *Tx) InsertEvalCases(ctx context.Context, cases []EvalCase) error {
	for i := range cases {
		c := &cases[i]
		c.ID, c.CreatedAt = model.NewID(), tx.now
		if _, err := tx.Exec(ctx, `INSERT INTO eval_cases (id, proposal_id, mode, case_name, pass, state, failure_reason, turns, cost_usd, details, created_at)
			VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
			c.ID, c.ProposalID, c.Mode, c.CaseName, boolInt(c.Pass), c.State, c.FailureReason, ptrInt(c.Turns), nullFloatPtr(c.CostUSD), c.Details, formatTime(c.CreatedAt)); err != nil {
			return fmt.Errorf("insert eval case %s/%s: %w", c.Mode, c.CaseName, err)
		}
	}
	return nil
}

// ListEvalCases returns per-case history newest first, optionally for one
// mode — the eval time series.
func (s *Store) ListEvalCases(ctx context.Context, mode string, limit int) ([]EvalCase, error) {
	if limit <= 0 || limit > 1000 {
		limit = 200
	}
	q := `SELECT id, proposal_id, mode, case_name, pass, state, failure_reason, turns, cost_usd, details, created_at FROM eval_cases`
	args := []any{}
	if mode != "" {
		q += ` WHERE mode = ?`
		args = append(args, mode)
	}
	q += ` ORDER BY created_at DESC, case_name LIMIT ?`
	args = append(args, limit)
	var out []EvalCase
	err := each(s.query(ctx, q, args...))(func(rows *sql.Rows) error {
		var c EvalCase
		var pass int
		var turns sql.NullInt64
		var cost sql.NullFloat64
		var created string
		if err := rows.Scan(&c.ID, &c.ProposalID, &c.Mode, &c.CaseName, &pass, &c.State, &c.FailureReason, &turns, &cost, &c.Details, &created); err != nil {
			return err
		}
		c.Pass = pass == 1
		c.Turns, c.CostUSD = intPtr(turns), floatPtr(cost)
		var err error
		if c.CreatedAt, err = parseTime(sql.NullString{String: created, Valid: true}); err != nil {
			return err
		}
		out = append(out, c)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("list eval cases: %w", err)
	}
	return out, nil
}

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
