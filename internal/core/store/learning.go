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
		WHERE w.routine_name IN ('reflect-library', 'learning-director') OR w.cause = 'promotion'
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
		WHERE (w.routine_name IN ('reflect-library', 'learning-director') OR w.cause = 'promotion'
		   OR EXISTS (SELECT 1 FROM work r WHERE r.id = f.root_work_id AND r.submitted_by LIKE 'bench:%'))
		  AND f.finished_at > ?`, formatTime(since)).Scan(&usd)
	if err != nil {
		return 0, fmt.Errorf("learning spend: %w", err)
	}
	return usd.Float64, nil
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
		WHERE (w.routine_name IN ('reflect-library', 'learning-director') OR w.cause = 'promotion'
		   OR EXISTS (SELECT 1 FROM work r WHERE r.id = f.root_work_id AND r.submitted_by LIKE 'bench:%'))
		  AND a.runner IN (`+marks+`)
		  AND f.finished_at > ?`, args...).Scan(&usd)
	if err != nil {
		return 0, fmt.Errorf("learning api spend: %w", err)
	}
	return usd.Float64, nil
}

// ConflictedTargets lists targets sitting in conflict since before the given
// instant — the conflict auto-recovery sweep's input.
func (s *Store) ConflictedTargets(ctx context.Context, olderThan time.Time) ([]Target, error) {
	return scanTargets(each(s.query(ctx, `SELECT `+targetColumns+` FROM targets WHERE state = 'conflict' AND updated_at < ?`, formatTime(olderThan))))
}

// DeadDependant is one open Work whose on:success dependency can never be
// satisfied: the blocker finished in a non-success state.
type DeadDependant struct {
	WorkID       string
	BlockedBy    string
	BlockerState string
}

// DeadSuccessDependants lists open Works blocked on:success of a finished
// blocker none of whose targets reached a success state (succeeded or
// merged) — permanent wedges the sweep cancels.
func (s *Store) DeadSuccessDependants(ctx context.Context) ([]DeadDependant, error) {
	rows, err := s.query(ctx, `
		SELECT d.work_id, d.blocked_by_work_id,
		       (SELECT GROUP_CONCAT(state) FROM targets WHERE work_id = d.blocked_by_work_id)
		FROM work_dependencies d
		JOIN work w  ON w.id  = d.work_id            AND w.finished_at IS NULL
		JOIN work wb ON wb.id = d.blocked_by_work_id AND wb.finished_at IS NOT NULL
		WHERE d."on" = 'success'
		  AND NOT EXISTS (SELECT 1 FROM targets t WHERE t.work_id = wb.id AND t.state IN ('succeeded', 'merged'))
		LIMIT 50`)
	var out []DeadDependant
	err = each(rows, err)(func(r *sql.Rows) error {
		var d DeadDependant
		var states sql.NullString
		if err := r.Scan(&d.WorkID, &d.BlockedBy, &states); err != nil {
			return err
		}
		d.BlockerState = states.String
		out = append(out, d)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("dead dependants: %w", err)
	}
	return out, nil
}

// LearningRun reads one self-improvement Work the way the Learning detail
// page needs it: the work row, its target's fate, and the newest attempt's
// result envelope. ok=false when the work does not exist.
func (s *Store) LearningRun(ctx context.Context, workID string) (*LearningWorkRow, error) {
	rows, err := s.query(ctx, `
		SELECT w.id, w.title, w.routine_name, w.cause, w.created_at,
		       t.state, t.unverified_reason, a.result, a.cost_usd
		FROM work w
		JOIN targets t ON t.work_id = w.id
		LEFT JOIN attempts a ON a.id = (
			SELECT id FROM attempts WHERE target_id = t.id ORDER BY started_at DESC LIMIT 1
		)
		WHERE w.id = ? LIMIT 1`, workID)
	var out *LearningWorkRow
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
		var perr error
		if row.CreatedAt, perr = parseTime(created); perr != nil {
			return perr
		}
		out = &row
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("learning run: %w", err)
	}
	return out, nil
}

// VerifyChildOf finds the verify follow-up Work for a subject Work (cause
// "verify", caused_by the subject) — the rebuttal half of a learning.
func (s *Store) VerifyChildOf(ctx context.Context, workID string) (string, error) {
	var id sql.NullString
	err := s.queryRow(ctx, `SELECT id FROM work WHERE caused_by_work_id = ? AND cause = 'verify' ORDER BY created_at DESC LIMIT 1`, workID).Scan(&id)
	if err == sql.ErrNoRows {
		return "", nil
	}
	return id.String, err
}

// RepoWorkAges reports a repository's open-work count and its newest
// finished_at — the bench reaper's "fully settled and stale" test.
func (s *Store) RepoWorkAges(ctx context.Context, repo string) (open int, newest time.Time, err error) {
	var newestStr sql.NullString
	err = s.queryRow(ctx, `
		SELECT COALESCE(SUM(CASE WHEN w.finished_at IS NULL THEN 1 ELSE 0 END), 0), MAX(w.finished_at)
		FROM work w JOIN targets t ON t.work_id = w.id WHERE t.repository_name = ?`, repo).Scan(&open, &newestStr)
	if err != nil {
		return 0, time.Time{}, fmt.Errorf("repo work ages: %w", err)
	}
	newest, _ = parseTime(newestStr)
	return open, newest, nil
}

// TreeScore returns the newest supervise overall score anywhere in a Work's
// tree (facts carry root_work_id), or nil when the tree was never assessed —
// the outcome an experiment on a PLANNING directive must be judged by: the
// plan-root's own verification only says the plan parsed; the supervise
// score says whether what the plan produced was any good.
func (s *Store) TreeScore(ctx context.Context, rootWorkID string) (*int, error) {
	var score sql.NullInt64
	err := s.queryRow(ctx, `
		SELECT score_overall FROM attempt_facts
		WHERE root_work_id = ? AND score_overall IS NOT NULL
		ORDER BY finished_at DESC LIMIT 1`, rootWorkID).Scan(&score)
	if err == sql.ErrNoRows || !score.Valid {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("tree score: %w", err)
	}
	v := int(score.Int64)
	return &v, nil
}
