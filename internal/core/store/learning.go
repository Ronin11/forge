package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"
)

// LearningWorkRow is one self-improvement Work for the Learning feed: a
// reflect-library run or a scratch-script promotion, with its target's fate
// and the newest attempt's result envelope. The feed classifies from the
// target state (merged = the edit landed; unverified with a verify_verdict
// reason = the verifier refuted it).
type LearningWorkRow struct {
	ID               string
	Title            string
	RoutineName      string
	Cause            string
	CreatedAt        time.Time
	State            string
	UnverifiedReason string
	Result           json.RawMessage
	CostUSD          *float64
}

// LearningWorks returns the self-improvement Works newest first: every
// reflect-library run plus every promotion-cause Work (scratch curation).
// One target per such Work by construction (both operate on one repository).
func (s *Store) LearningWorks(ctx context.Context, limit int) ([]LearningWorkRow, error) {
	if limit <= 0 || limit > 500 {
		limit = 100
	}
	rows, err := s.query(ctx, `
		SELECT w.id, w.title, w.routine_name, w.cause, w.created_at,
		       t.state, t.unverified_reason, a.result, a.cost_usd
		FROM work w
		JOIN targets t ON t.work_id = w.id
		LEFT JOIN attempts a ON a.id = (
			SELECT id FROM attempts WHERE target_id = t.id ORDER BY started_at DESC LIMIT 1
		)
		WHERE w.routine_name = 'reflect-library' OR w.cause = 'promotion'
		ORDER BY w.created_at DESC LIMIT ?`, limit)
	var out []LearningWorkRow
	err = each(rows, err)(func(r *sql.Rows) error {
		var row LearningWorkRow
		var cause, unverified, result, created sql.NullString
		var cost sql.NullFloat64
		if err := r.Scan(&row.ID, &row.Title, &row.RoutineName, &cause, &created, &row.State, &unverified, &result, &cost); err != nil {
			return err
		}
		row.Cause, row.UnverifiedReason = cause.String, unverified.String
		row.Result = rawOrNil(result)
		if cost.Valid {
			v := cost.Float64
			row.CostUSD = &v
		}
		var err error
		if row.CreatedAt, err = parseTime(created); err != nil {
			return err
		}
		out = append(out, row)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("learning works: %w", err)
	}
	return out, nil
}

// LearningSpendSince sums what self-improvement cost since the given instant:
// every fact whose Work is a reflect-library run or a promotion — the ledger
// behind [learning] usd_per_week. Experiment optimizer calls that never
// become attempts are not counted yet (they have no facts row).
func (s *Store) LearningSpendSince(ctx context.Context, since time.Time) (float64, error) {
	var usd sql.NullFloat64
	err := s.queryRow(ctx, `
		SELECT SUM(f.cost_usd) FROM attempt_facts f
		JOIN work w ON w.id = f.work_id
		WHERE (w.routine_name = 'reflect-library' OR w.cause = 'promotion')
		  AND f.finished_at > ?`, formatTime(since)).Scan(&usd)
	if err != nil {
		return 0, fmt.Errorf("learning spend: %w", err)
	}
	return usd.Float64, nil
}

// CountExperimentAssignments reports how many Works were ever stamped with
// this experiment — the round-robin cursor's seed after a daemon restart, so
// redeploys don't reset arm rotation back to control every time.
func (s *Store) CountExperimentAssignments(ctx context.Context, id string) (int, error) {
	var n int
	err := s.queryRow(ctx, `SELECT COUNT(*) FROM work WHERE json_extract(composition, '$.experiment') = ?`, id).Scan(&n)
	return n, err
}

// RecentExperiments lists experiments across every subject, newest first —
// the Learning feed's experiment entries (Experiments is per-subject and
// capped at the retention window).
func (s *Store) RecentExperiments(ctx context.Context, limit int) ([]Experiment, error) {
	if limit <= 0 || limit > 500 {
		limit = 100
	}
	return scanExperiments(each(s.query(ctx, `SELECT `+experimentColumns+` FROM experiments ORDER BY created_at DESC LIMIT ?`, limit)))
}
