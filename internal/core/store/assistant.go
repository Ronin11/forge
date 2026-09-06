package store

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	"forge/internal/core/model"
)

// AssistantTurn is one exchange of the sessionized chat front door.
type AssistantTurn struct {
	ID            string    `json:"id"`
	Sender        string    `json:"sender"`
	UserText      string    `json:"user_text"`
	AssistantText string    `json:"assistant_text"`
	Action        string    `json:"action,omitempty"`
	Ref           string    `json:"ref,omitempty"` // work:<id> | question:<id>
	CreatedAt     time.Time `json:"created_at"`
}

// InsertAssistantTurn records one exchange.
func (tx *Tx) InsertAssistantTurn(ctx context.Context, t *AssistantTurn) error {
	if t.ID == "" {
		t.ID = model.NewID()
	}
	t.CreatedAt = tx.now
	if _, err := tx.Exec(ctx, `INSERT INTO assistant_turns (id, sender, user_text, assistant_text, action, ref, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)`,
		t.ID, t.Sender, t.UserText, t.AssistantText, t.Action, t.Ref, formatTime(t.CreatedAt)); err != nil {
		return fmt.Errorf("insert assistant turn: %w", err)
	}
	return nil
}

// RecentAssistantTurns lists a sender's turns, newest first.
func (s *Store) RecentAssistantTurns(ctx context.Context, sender string, limit int) ([]AssistantTurn, error) {
	if limit <= 0 || limit > 200 {
		limit = 40
	}
	rows, err := s.query(ctx, `SELECT id, sender, user_text, assistant_text, action, ref, created_at FROM assistant_turns WHERE sender = ? ORDER BY created_at DESC, id DESC LIMIT ?`, sender, limit)
	var out []AssistantTurn
	err = each(rows, err)(func(r *sql.Rows) error {
		var t AssistantTurn
		var created string
		if err := r.Scan(&t.ID, &t.Sender, &t.UserText, &t.AssistantText, &t.Action, &t.Ref, &created); err != nil {
			return err
		}
		var perr error
		if t.CreatedAt, perr = parseTime(sql.NullString{String: created, Valid: true}); perr != nil {
			return perr
		}
		out = append(out, t)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("assistant turns: %w", err)
	}
	return out, nil
}
