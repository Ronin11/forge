package store

// The store methods the MCP tool layer (internal/tools and the /api/v1/tools
// handlers) needs beyond the general API.

import (
	"context"
	"fmt"

	"forge/internal/kb"
)

// RebindMCPToken stores the hash of a re-minted per-attempt MCP token. A
// replayed claim cannot return the original token — only its hash is stored —
// so the daemon mints a fresh one and rebinds it here, in the claim's
// transaction, so the token it returns is the one forge mcp may present.
func (tx *Tx) RebindMCPToken(ctx context.Context, attemptID, token string) error {
	res, err := tx.Exec(ctx, `UPDATE attempts SET mcp_token_hash = ?, updated_at = ? WHERE id = ?`,
		HashToken(token), formatTime(tx.now), attemptID)
	if err != nil {
		return fmt.Errorf("rebind mcp token of %s: %w", attemptID, err)
	}
	if err := oneRow(res, "attempt "+attemptID); err != nil {
		return err
	}
	return tx.Journal(ctx, "attempt.mcp_rebound", EntityAttempt, attemptID, nil)
}

// IndexKbNote indexes exactly one note. ReindexKb given a partial list drops
// every id not in it (vanished files), so a single-note write (forge_kb_new)
// must come through here instead.
func (tx *Tx) IndexKbNote(ctx context.Context, n *kb.Note) error {
	return tx.indexNote(ctx, n)
}

// NextControlSeq returns the next free seq for control-source events of an
// attempt. (attempt, source, seq) is the idempotency key of InsertEvents, so
// the daemon's own events must never reuse a seq.
func (tx *Tx) NextControlSeq(ctx context.Context, attemptID string) (int, error) {
	var n int
	if err := tx.QueryRow(ctx, `SELECT COALESCE(MAX(seq)+1, 0) FROM events WHERE attempt_id = ? AND source = 'control'`, attemptID).Scan(&n); err != nil {
		return 0, fmt.Errorf("next control seq of %s: %w", attemptID, err)
	}
	return n, nil
}
