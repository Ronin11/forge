package store

import (
	"context"
	"database/sql"
	"fmt"
	"time"
)

// RetainedWorktreeCount is how many worktrees workers have reported as kept on
// disk; doctor surfaces the number so an operator notices the pile.
func (s *Store) RetainedWorktreeCount(ctx context.Context) (int, error) {
	var n int
	if err := s.queryRow(ctx, `SELECT count(*) FROM retained_worktrees`).Scan(&n); err != nil {
		return 0, fmt.Errorf("count retained worktrees: %w", err)
	}
	return n, nil
}

// RetainedWorktree is one kept worktree, resolved to the repository it belongs
// to through its attempt's target. The repository detail page lists them with
// their cleanup commands.
type RetainedWorktree struct {
	AttemptID      string `json:"attempt_id"`
	Path           string `json:"path"`
	Reason         string `json:"reason"`
	CleanupCommand string `json:"cleanup_command"`
}

// RetainedWorktreesForRepository lists the worktrees workers kept for one
// repository, newest first, so the repository page can show the cleanup hint.
func (s *Store) RetainedWorktreesForRepository(ctx context.Context, repo string) ([]RetainedWorktree, error) {
	var out []RetainedWorktree
	err := each(s.query(ctx, `SELECT rw.attempt_id, rw.path, rw.reason, rw.cleanup_command
		FROM retained_worktrees rw
		JOIN attempts a ON a.id = rw.attempt_id
		JOIN targets t ON t.id = a.target_id
		WHERE t.repository_name = ?
		ORDER BY rw.reported_at DESC`, repo))(func(rows *sql.Rows) error {
		var r RetainedWorktree
		if err := rows.Scan(&r.AttemptID, &r.Path, &r.Reason, &r.CleanupCommand); err != nil {
			return fmt.Errorf("scan retained worktree: %w", err)
		}
		out = append(out, r)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read retained worktrees for %s: %w", repo, err)
	}
	return out, nil
}

// KbLastIndexedAt is when the newest kb note row was (re)indexed; zero when
// nothing is indexed. Doctor uses it to spot a stuck reindex loop.
func (s *Store) KbLastIndexedAt(ctx context.Context) (time.Time, error) {
	var raw sql.NullString
	if err := s.queryRow(ctx, `SELECT max(indexed_at) FROM kb_notes`).Scan(&raw); err != nil {
		return time.Time{}, fmt.Errorf("read kb index age: %w", err)
	}
	if !raw.Valid || raw.String == "" {
		return time.Time{}, nil
	}
	return parseTime(raw)
}
