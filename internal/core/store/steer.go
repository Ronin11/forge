package store

// Steer (M11, DESIGN §22): pending operator turns for a running attempt ride
// the journal. The attempt.steer row written at enqueue is both the audit
// trail and the storage — no new table, no migration, and a daemon restart
// never loses an undelivered steer. Delivery is marked by one
// attempt.steer_delivered row whose payload records the last delivered
// journal id; steer rows after that id are pending.

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
)

// EnqueueSteer queues one operator turn for an attempt. The text lives in the
// journal payload the way a question's answer lives in its row: it is what
// the human said, part of the audit trail.
func (tx *Tx) EnqueueSteer(ctx context.Context, attemptID, text string) error {
	return tx.Journal(ctx, "attempt.steer", EntityAttempt, attemptID, map[string]any{"text": text, "bytes": len(text)})
}

// TakeSteers returns the attempt's undelivered steer texts, oldest first, and
// marks them delivered in the same transaction — handing them to the
// heartbeat response is the delivery.
func (tx *Tx) TakeSteers(ctx context.Context, attemptID string) ([]string, error) {
	var texts []string
	var lastID int64
	err := each(tx.Query(ctx, `SELECT id, payload FROM journal
		WHERE entity_type = ? AND entity_id = ? AND kind = 'attempt.steer'
		AND id > COALESCE((SELECT max(id) FROM journal WHERE entity_type = ? AND entity_id = ? AND kind = 'attempt.steer_delivered'), 0)
		ORDER BY id`, EntityAttempt, attemptID, EntityAttempt, attemptID))(func(rows *sql.Rows) error {
		var id int64
		var payload string
		if err := rows.Scan(&id, &payload); err != nil {
			return err
		}
		var body struct {
			Text string `json:"text"`
		}
		if err := json.Unmarshal([]byte(payload), &body); err != nil {
			return fmt.Errorf("steer row %d: %w", id, err)
		}
		texts, lastID = append(texts, body.Text), id
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read pending steers of %s: %w", attemptID, err)
	}
	if len(texts) == 0 {
		return nil, nil
	}
	if err := tx.Journal(ctx, "attempt.steer_delivered", EntityAttempt, attemptID, map[string]any{"through": lastID, "count": len(texts)}); err != nil {
		return nil, err
	}
	return texts, nil
}
