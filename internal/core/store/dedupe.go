package store

// Dedupe lookups (M11, DESIGN §22): intake refuses a prompt already submitted
// within the window, and the forge.toml bootstrap proposal fires at most once
// per repository.

import (
	"context"
	"fmt"
	"time"
)

// WorkByPromptHash returns Work rows sharing a prompt hash created at or
// after since, newest first — intake dedupe's input. It reads the pool:
// dedupe is advisory (a --force exists) and the race window is one request.
func (s *Store) WorkByPromptHash(ctx context.Context, hash string, since time.Time) ([]Work, error) {
	return scanWork(each(s.query(ctx, `SELECT `+workColumns+` FROM work WHERE prompt_hash = ? AND created_at >= ? ORDER BY created_at DESC`, hash, formatTime(since))))
}

// HasProposalForTarget reports whether any proposal, whatever its status,
// names target. The bootstrap proposal dedupes on it so a repository is
// proposed a forge.toml exactly once — a human's rejection is not overridden
// by the next registration tick.
func (tx *Tx) HasProposalForTarget(ctx context.Context, target string) (bool, error) {
	var n int
	if err := tx.QueryRow(ctx, `SELECT count(*) FROM proposals WHERE target = ?`, target).Scan(&n); err != nil {
		return false, fmt.Errorf("count proposals for %s: %w", target, err)
	}
	return n > 0, nil
}
