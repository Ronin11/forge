package store

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	"forge/internal/core/model"
)

// Merge is one Target's trip through the merge queue (DESIGN.md §20): who,
// which integration branch, the before/after SHAs of the push, how many
// rebase attempts it took, and the outcome (merged | conflict |
// checks_failed). One row per Target; rebases increment in place.
type Merge struct {
	ID                string    `json:"id"`
	TargetID          string    `json:"target_id"`
	Repository        string    `json:"repository"`
	IntegrationBranch string    `json:"integration_branch"`
	BeforeSHA         string    `json:"before_sha,omitempty"`
	AfterSHA          string    `json:"after_sha,omitempty"`
	RebaseAttempts    int       `json:"rebase_attempts"`
	Outcome           string    `json:"outcome,omitempty"`
	PushedAt          time.Time `json:"pushed_at,omitempty"`
	CreatedAt         time.Time `json:"created_at"`
}

// BeginMerge finds or creates the Target's merges row and increments its
// rebase attempts, journaling the start. It is called each time the
// integrator picks the Target up.
func (tx *Tx) BeginMerge(ctx context.Context, targetID, repository, branch string) (*Merge, error) {
	m, err := scanMerges(each(tx.Query(ctx, mergeSelect+` WHERE target_id = ?`, targetID)))
	if err != nil {
		return nil, err
	}
	if len(m) == 0 {
		row := Merge{ID: model.NewID(), TargetID: targetID, Repository: repository, IntegrationBranch: branch, RebaseAttempts: 1, CreatedAt: tx.now}
		if _, err := tx.Exec(ctx, `INSERT INTO merges (id, target_id, repository_name, integration_branch, rebase_attempts, created_at) VALUES (?, ?, ?, ?, 1, ?)`,
			row.ID, row.TargetID, row.Repository, row.IntegrationBranch, formatTime(tx.now)); err != nil {
			return nil, fmt.Errorf("insert merge for %s: %w", targetID, err)
		}
		if err := tx.Journal(ctx, "merge.started", EntityMerge, row.ID, map[string]any{"target_id": targetID, "repository": repository, "branch": branch, "rebase_attempts": 1}); err != nil {
			return nil, err
		}
		return &row, nil
	}
	row := m[0]
	row.RebaseAttempts++
	row.IntegrationBranch = branch
	if _, err := tx.Exec(ctx, `UPDATE merges SET rebase_attempts = rebase_attempts + 1, integration_branch = ?, outcome = NULL WHERE id = ?`, branch, row.ID); err != nil {
		return nil, fmt.Errorf("bump merge %s: %w", row.ID, err)
	}
	if err := tx.Journal(ctx, "merge.started", EntityMerge, row.ID, map[string]any{"target_id": targetID, "repository": repository, "branch": branch, "rebase_attempts": row.RebaseAttempts}); err != nil {
		return nil, err
	}
	return &row, nil
}

// FinishMerge records the outcome. A merged outcome carries the pushed
// before/after SHAs and journals merge.pushed — the constitution-10 audit row.
func (tx *Tx) FinishMerge(ctx context.Context, id, outcome, before, after string, detail map[string]any) error {
	set, args := `outcome = ?`, []any{outcome}
	if outcome == "merged" {
		set += `, before_sha = ?, after_sha = ?, pushed_at = ?`
		args = append(args, before, after, formatTime(tx.now))
	}
	args = append(args, id)
	res, err := tx.Exec(ctx, `UPDATE merges SET `+set+` WHERE id = ?`, args...)
	if err != nil {
		return fmt.Errorf("finish merge %s: %w", id, err)
	}
	if err := oneRow(res, "merge "+id); err != nil {
		return err
	}
	payload := map[string]any{"outcome": outcome, "before": before, "after": after}
	for k, v := range detail {
		payload[k] = v
	}
	kind := "merge." + outcome
	if outcome == "merged" {
		kind = "merge.pushed"
	}
	return tx.Journal(ctx, kind, EntityMerge, id, payload)
}

// MergeForTarget reads a Target's merge row, or nil.
func (s *Store) MergeForTarget(ctx context.Context, targetID string) (*Merge, error) {
	m, err := scanMerges(each(s.query(ctx, mergeSelect+` WHERE target_id = ?`, targetID)))
	if err != nil {
		return nil, err
	}
	if len(m) == 0 {
		return nil, nil
	}
	return &m[0], nil
}

// TargetsInState lists Targets in one state, oldest transition first — the
// integrator's queue view (queued_for_merge) and its crash recovery (merging).
func (s *Store) TargetsInState(ctx context.Context, state model.State) ([]Target, error) {
	return scanTargets(each(s.query(ctx, `SELECT `+targetColumns+` FROM targets WHERE state = ? ORDER BY updated_at, id`, string(state))))
}

const mergeSelect = `SELECT id, target_id, repository_name, integration_branch, before_sha, after_sha, rebase_attempts, outcome, pushed_at, created_at FROM merges`

func scanMerges(iter func(func(*sql.Rows) error) error) ([]Merge, error) {
	var out []Merge
	err := iter(func(rows *sql.Rows) error {
		var m Merge
		var before, after, outcome, pushed sql.NullString
		var created string
		if err := rows.Scan(&m.ID, &m.TargetID, &m.Repository, &m.IntegrationBranch, &before, &after, &m.RebaseAttempts, &outcome, &pushed, &created); err != nil {
			return fmt.Errorf("scan merge: %w", err)
		}
		m.BeforeSHA, m.AfterSHA, m.Outcome = before.String, after.String, outcome.String
		var err error
		if m.PushedAt, err = parseTime(pushed); err != nil {
			return err
		}
		if m.CreatedAt, err = parseTime(sql.NullString{String: created, Valid: true}); err != nil {
			return err
		}
		out = append(out, m)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read merges: %w", err)
	}
	return out, nil
}
