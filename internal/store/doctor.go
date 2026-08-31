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
