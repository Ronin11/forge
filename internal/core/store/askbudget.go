package store

import (
	"context"
	"fmt"
)

// QuestionCount counts every question ever asked on a Target, across all of
// its attempts — the ask-budget rule's input (routines.max_questions,
// DESIGN §22): a new question when the count has reached the budget fails the
// Target with ask_budget_exhausted.
func (tx *Tx) QuestionCount(ctx context.Context, targetID string) (int, error) {
	var n int
	if err := tx.QueryRow(ctx, `SELECT count(*) FROM questions WHERE target_id = ?`, targetID).Scan(&n); err != nil {
		return 0, fmt.Errorf("count questions of %s: %w", targetID, err)
	}
	return n, nil
}
