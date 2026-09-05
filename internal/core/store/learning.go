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

// SuperviseAssessment is one recorded supervise verdict, resolved to the ask
// it judged — the evidence reflection needs that per-directive stats can't
// carry: what the finished product was actually missing.
type SuperviseAssessment struct {
	At         time.Time       `json:"at"`
	RootWorkID string          `json:"root_work_id,omitempty"`
	RootTitle  string          `json:"root_title,omitempty"`
	Repository string          `json:"repository,omitempty"`
	Outcome    string          `json:"outcome"`
	Round      int             `json:"round"`
	Scores     json.RawMessage `json:"scores,omitempty"`
	Weakness   string          `json:"weakness,omitempty"`
}

// RecentAssessments lists supervise verdicts since the given instant, newest
// first, each joined to its root Work (the original ask) and repository.
func (s *Store) RecentAssessments(ctx context.Context, since time.Time, limit int) ([]SuperviseAssessment, error) {
	if limit <= 0 || limit > 100 {
		limit = 20
	}
	rows, err := s.query(ctx, `
		SELECT j.ts, j.payload, w.root_work_id, COALESCE(rw.title, w.title), COALESCE(t.repository_name, '')
		FROM journal j
		JOIN work w ON w.id = j.entity_id
		LEFT JOIN work rw ON rw.id = w.root_work_id
		LEFT JOIN targets t ON t.work_id = w.id
		WHERE j.kind = 'supervise.assessment' AND j.ts > ?
		ORDER BY j.id DESC LIMIT ?`, formatTime(since), limit)
	var out []SuperviseAssessment
	err = each(rows, err)(func(r *sql.Rows) error {
		var ts, payload, title, repo string
		var root sql.NullString
		if err := r.Scan(&ts, &payload, &root, &title, &repo); err != nil {
			return err
		}
		var body struct {
			Outcome  string          `json:"outcome"`
			Round    int             `json:"round"`
			Scores   json.RawMessage `json:"scores"`
			Weakness string          `json:"weakness"`
		}
		if err := json.Unmarshal([]byte(payload), &body); err != nil {
			return nil // a malformed old row never breaks the pack
		}
		a := SuperviseAssessment{RootWorkID: root.String, RootTitle: title, Repository: repo,
			Outcome: body.Outcome, Round: body.Round, Scores: body.Scores, Weakness: body.Weakness}
		if t, err := parseTime(sql.NullString{String: ts, Valid: true}); err == nil {
			a.At = t
		}
		out = append(out, a)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("recent assessments: %w", err)
	}
	return out, nil
}

// LearningSpendSince sums what self-improvement cost since the given instant:
// every fact whose Work is a reflect-library run, a promotion, or part of a
// benchmark tree (measurement is R&D too) — the ledger behind [learning]
// usd_per_week. Experiment optimizer calls that never become attempts are
// not counted yet (they have no facts row).
func (s *Store) LearningSpendSince(ctx context.Context, since time.Time) (float64, error) {
	var usd sql.NullFloat64
	err := s.queryRow(ctx, `
		SELECT SUM(f.cost_usd) FROM attempt_facts f
		JOIN work w ON w.id = f.work_id
		WHERE (w.routine_name = 'reflect-library' OR w.cause = 'promotion'
		   OR EXISTS (SELECT 1 FROM work r WHERE r.id = f.root_work_id AND r.submitted_by LIKE 'bench:%'))
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

// OpenWorkCountForSubmitter counts unfinished Works with the given
// submitted_by — the bench scheduler's skip-if-running key.
func (s *Store) OpenWorkCountForSubmitter(ctx context.Context, submittedBy string) (int, error) {
	var n int
	err := s.queryRow(ctx, `SELECT COUNT(*) FROM work WHERE submitted_by = ? AND finished_at IS NULL`, submittedBy).Scan(&n)
	return n, err
}

// LearningAPISpendSince is LearningSpendSince restricted to attempts that ran
// on API-billed runners — the only spend that is real marginal dollars. With
// no API-billed runners configured it is always zero: subscription tokens are
// prepaid (use-it-or-lose-it) and are governed by window capacity, not USD.
func (s *Store) LearningAPISpendSince(ctx context.Context, since time.Time, apiRunners []string) (float64, error) {
	if len(apiRunners) == 0 {
		return 0, nil
	}
	args := []any{}
	marks := ""
	for i, r := range apiRunners {
		if i > 0 {
			marks += ","
		}
		marks += "?"
		args = append(args, r)
	}
	args = append(args, formatTime(since))
	var usd sql.NullFloat64
	err := s.queryRow(ctx, `
		SELECT SUM(f.cost_usd) FROM attempt_facts f
		JOIN work w ON w.id = f.work_id
		JOIN attempts a ON a.id = f.attempt_id
		WHERE (w.routine_name = 'reflect-library' OR w.cause = 'promotion'
		   OR EXISTS (SELECT 1 FROM work r WHERE r.id = f.root_work_id AND r.submitted_by LIKE 'bench:%'))
		  AND a.runner IN (`+marks+`)
		  AND f.finished_at > ?`, args...).Scan(&usd)
	if err != nil {
		return 0, fmt.Errorf("learning api spend: %w", err)
	}
	return usd.Float64, nil
}
