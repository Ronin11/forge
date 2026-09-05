package store

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	"forge/internal/core/model"
)

// Prediction is one recorded, resolvable judgment: who claimed what would
// hold, with what confidence, and how it turned out. Calibration over these
// rows is the objective trust measure behind the autonomy ladder.
type Prediction struct {
	ID          string    `json:"id"`
	Source      string    `json:"source"`
	SourceRef   string    `json:"source_ref"`
	ProposalID  string    `json:"proposal_id,omitempty"`
	Subject     string    `json:"subject"`
	Statement   string    `json:"statement"`
	Probability *float64  `json:"probability,omitempty"`
	CreatedAt   time.Time `json:"created_at"`
	ResolveBy   time.Time `json:"resolve_by"`
	ResolvedAt  time.Time `json:"resolved_at,omitzero"`
	Outcome     *bool     `json:"outcome,omitempty"`
	Note        string    `json:"note,omitempty"`
}

// InsertPrediction records one open prediction.
func (tx *Tx) InsertPrediction(ctx context.Context, p *Prediction) error {
	if p.ID == "" {
		p.ID = model.NewID()
	}
	p.CreatedAt = tx.now
	var prob any
	if p.Probability != nil {
		prob = *p.Probability
	}
	_, err := tx.Exec(ctx, `INSERT INTO predictions (id, source, source_ref, proposal_id, subject, statement, probability, created_at, resolve_by, note) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		p.ID, p.Source, p.SourceRef, p.ProposalID, p.Subject, p.Statement, prob, formatTime(p.CreatedAt), formatTime(p.ResolveBy), p.Note)
	if err != nil {
		return fmt.Errorf("insert prediction: %w", err)
	}
	return tx.Journal(ctx, "prediction.recorded", EntityDaemon, p.ID, map[string]any{"source": p.Source, "subject": p.Subject, "statement": p.Statement, "probability": p.Probability})
}

// ResolvePrediction closes one prediction. outcome nil marks it unresolvable
// (excluded from calibration) with the note saying why.
func (tx *Tx) ResolvePrediction(ctx context.Context, id string, outcome *bool, note string) error {
	var out any
	if outcome != nil {
		out = boolInt(*outcome)
	}
	res, err := tx.Exec(ctx, `UPDATE predictions SET resolved_at = ?, outcome = ?, note = ? WHERE id = ? AND resolved_at IS NULL`,
		formatTime(tx.now), out, note, id)
	if err != nil {
		return fmt.Errorf("resolve prediction %s: %w", id, err)
	}
	if err := oneRow(res, "prediction "+id); err != nil {
		return err
	}
	return tx.Journal(ctx, "prediction.resolved", EntityDaemon, id, map[string]any{"outcome": outcome, "note": note})
}

// DuePredictions lists open predictions whose resolve_by has passed, plus
// every open prediction tied to a proposal (an early revert resolves before
// the deadline).
func (s *Store) DuePredictions(ctx context.Context, now time.Time) ([]Prediction, error) {
	return scanPredictions(each(s.query(ctx, `
		SELECT id, source, source_ref, proposal_id, subject, statement, probability, created_at, resolve_by, resolved_at, outcome, note
		FROM predictions WHERE resolved_at IS NULL AND (resolve_by <= ? OR proposal_id != '')
		ORDER BY created_at LIMIT 200`, formatTime(now))))
}

// RecentPredictions lists newest first, resolved or not (the Learning feed).
func (s *Store) RecentPredictions(ctx context.Context, limit int) ([]Prediction, error) {
	if limit <= 0 || limit > 200 {
		limit = 50
	}
	return scanPredictions(each(s.query(ctx, `
		SELECT id, source, source_ref, proposal_id, subject, statement, probability, created_at, resolve_by, resolved_at, outcome, note
		FROM predictions ORDER BY created_at DESC LIMIT ?`, limit)))
}

func scanPredictions(iter func(func(*sql.Rows) error) error) ([]Prediction, error) {
	var out []Prediction
	err := iter(func(rows *sql.Rows) error {
		var p Prediction
		var prob sql.NullFloat64
		var created, resolveBy string
		var resolved, note sql.NullString
		var outcome sql.NullInt64
		if err := rows.Scan(&p.ID, &p.Source, &p.SourceRef, &p.ProposalID, &p.Subject, &p.Statement, &prob, &created, &resolveBy, &resolved, &outcome, &note); err != nil {
			return fmt.Errorf("scan prediction: %w", err)
		}
		if prob.Valid {
			v := prob.Float64
			p.Probability = &v
		}
		if outcome.Valid {
			v := outcome.Int64 == 1
			p.Outcome = &v
		}
		p.Note = note.String
		var err error
		if p.CreatedAt, err = parseTime(sql.NullString{String: created, Valid: true}); err != nil {
			return err
		}
		if p.ResolveBy, err = parseTime(sql.NullString{String: resolveBy, Valid: true}); err != nil {
			return err
		}
		if p.ResolvedAt, err = parseTime(resolved); err != nil {
			return err
		}
		out = append(out, p)
		return nil
	})
	return out, err
}

// CalibrationRow is one source's track record: how often its predictions
// held, and (where confidences were stated) the Brier score — lower is
// better, 0.25 is what ignorant coin-flipping earns.
type CalibrationRow struct {
	Source   string   `json:"source"`
	Open     int      `json:"open"`
	Resolved int      `json:"resolved"` // with a definite outcome
	Held     int      `json:"held"`
	Brier    *float64 `json:"brier,omitempty"`
}

// Calibration aggregates the ledger per source since the given instant.
func (s *Store) Calibration(ctx context.Context, since time.Time) ([]CalibrationRow, error) {
	rows, err := s.query(ctx, `
		SELECT source,
		       SUM(CASE WHEN resolved_at IS NULL THEN 1 ELSE 0 END),
		       SUM(CASE WHEN outcome IS NOT NULL THEN 1 ELSE 0 END),
		       SUM(CASE WHEN outcome = 1 THEN 1 ELSE 0 END),
		       AVG(CASE WHEN outcome IS NOT NULL AND probability IS NOT NULL
		                THEN (probability - outcome) * (probability - outcome) END)
		FROM predictions WHERE created_at > ?
		GROUP BY source ORDER BY source`, formatTime(since))
	var out []CalibrationRow
	err = each(rows, err)(func(r *sql.Rows) error {
		var row CalibrationRow
		var brier sql.NullFloat64
		if err := r.Scan(&row.Source, &row.Open, &row.Resolved, &row.Held, &brier); err != nil {
			return err
		}
		if brier.Valid {
			row.Brier = &brier.Float64
		}
		out = append(out, row)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("calibration: %w", err)
	}
	return out, nil
}
