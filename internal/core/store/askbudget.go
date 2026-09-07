package store

import (
	"context"
	"fmt"
)

// QuestionCount counts the questions asked during one attempt — its launches
// and resumed checkpoint sessions share the attempt id, so a runaway asker
// within a single run still hits the cap. The budget is deliberately NOT
// counted across a Target's whole life: a retry is a fresh attempt with a
// fresh budget, because an inherited spent budget quietly doomed any
// approval-gated task's second life (the crashbyforge registration,
// 2026-09-07 — its retry filed one legitimate approval question and was
// failed for it).
func (tx *Tx) QuestionCount(ctx context.Context, attemptID string) (int, error) {
	var n int
	if err := tx.QueryRow(ctx, `SELECT count(*) FROM questions WHERE attempt_id = ?`, attemptID).Scan(&n); err != nil {
		return 0, fmt.Errorf("count questions of attempt %s: %w", attemptID, err)
	}
	return n, nil
}
