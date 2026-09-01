package store

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	"forge/internal/model"
)

// AttemptProgress is the live in-flight view of a running attempt: a turn/token
// tally derived from the events as they arrive, the latest heartbeat phase, and
// the most recent forge_note_progress note. It is written by the event,
// heartbeat, and note_progress paths and read only while the attempt is
// unfinished (the attempts row holds the authoritative totals once complete).
type AttemptProgress struct {
	RunningTurns int         `json:"running_turns"`
	TokensIn     int64       `json:"tokens_in"`
	TokensOut    int64       `json:"tokens_out"`
	LastEventAt  time.Time   `json:"last_event_at,omitempty"`
	State        model.State `json:"state,omitempty"`
	Phase        string      `json:"phase,omitempty"`
	PhaseAt      time.Time   `json:"phase_at,omitempty"`
	Note         string      `json:"last_progress_note,omitempty"`
	Checkpoint   string      `json:"checkpoint,omitempty"`
	NoteAt       time.Time   `json:"note_at,omitempty"`
}

// RecomputeAttemptProgress rebuilds the turn/token tally, last-event time, and
// latest progress note for an attempt from its events, then upserts them onto
// attempt_progress. Deriving from the events table (rather than accumulating
// deltas) keeps the tally correct under at-least-once event delivery: a
// redelivered batch recomputes the same numbers. The heartbeat columns are left
// untouched — they belong to RecordHeartbeat.
func (tx *Tx) RecomputeAttemptProgress(ctx context.Context, attemptID string) error {
	var turns int
	var in, out sql.NullInt64
	var lastEvent sql.NullString
	// One "usage" metric is emitted per de-duplicated assistant message, so a
	// count of them is the running turn count; their attrs carry the per-turn
	// token fields the parser stamped.
	row := tx.QueryRow(ctx, `SELECT
		COUNT(*),
		SUM(CAST(json_extract(attrs, '$.input_tokens') AS INTEGER)),
		SUM(CAST(json_extract(attrs, '$.output_tokens') AS INTEGER))
		FROM events WHERE attempt_id = ? AND kind = ? AND name = 'usage'`, attemptID, "metric")
	if err := row.Scan(&turns, &in, &out); err != nil {
		return fmt.Errorf("tally usage for %s: %w", attemptID, err)
	}
	if err := tx.QueryRow(ctx, `SELECT MAX(time) FROM events WHERE attempt_id = ?`, attemptID).Scan(&lastEvent); err != nil {
		return fmt.Errorf("last event for %s: %w", attemptID, err)
	}
	// The most recent forge_note_progress note is the latest lifecycle "progress"
	// event; its checkpoint, when the note declared one, rides the attrs.
	var note, checkpoint, noteAt sql.NullString
	err := tx.QueryRow(ctx, `SELECT message, json_extract(attrs, '$.checkpoint'), time
		FROM events WHERE attempt_id = ? AND kind = ? AND name = 'progress'
		ORDER BY time DESC, source DESC, seq DESC LIMIT 1`, attemptID, "lifecycle").Scan(&note, &checkpoint, &noteAt)
	if err != nil && err != sql.ErrNoRows {
		return fmt.Errorf("progress note for %s: %w", attemptID, err)
	}
	_, err = tx.Exec(ctx, `INSERT INTO attempt_progress
		(attempt_id, running_turns, input_tokens, output_tokens, last_event_at, progress_note, checkpoint, progress_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(attempt_id) DO UPDATE SET
			running_turns = excluded.running_turns,
			input_tokens = excluded.input_tokens,
			output_tokens = excluded.output_tokens,
			last_event_at = excluded.last_event_at,
			progress_note = excluded.progress_note,
			checkpoint = excluded.checkpoint,
			progress_at = excluded.progress_at,
			updated_at = excluded.updated_at`,
		attemptID, turns, in.Int64, out.Int64, nullSQLString(lastEvent), nullSQLString(note), nullSQLString(checkpoint), nullSQLString(noteAt), formatTime(tx.now))
	if err != nil {
		return fmt.Errorf("upsert progress for %s: %w", attemptID, err)
	}
	return nil
}

// RecordHeartbeatProgress upserts the latest heartbeat State and Phase onto
// attempt_progress, stamping last_heartbeat_at. It touches only the heartbeat
// columns; the tally and note belong to RecomputeAttemptProgress.
func (tx *Tx) RecordHeartbeatProgress(ctx context.Context, attemptID string, state model.State, phase string) error {
	_, err := tx.Exec(ctx, `INSERT INTO attempt_progress
		(attempt_id, hb_state, phase, last_heartbeat_at, updated_at)
		VALUES (?, ?, ?, ?, ?)
		ON CONFLICT(attempt_id) DO UPDATE SET
			hb_state = excluded.hb_state,
			phase = excluded.phase,
			last_heartbeat_at = excluded.last_heartbeat_at,
			updated_at = excluded.updated_at`,
		attemptID, nullString(string(state)), nullString(phase), formatTime(tx.now), formatTime(tx.now))
	if err != nil {
		return fmt.Errorf("upsert heartbeat progress for %s: %w", attemptID, err)
	}
	return nil
}

// AttemptProgress reads the live progress row for an attempt, returning nil when
// none has been written yet (nothing has arrived for it).
func (s *Store) AttemptProgress(ctx context.Context, attemptID string) (*AttemptProgress, error) {
	var p AttemptProgress
	var lastEvent, hbState, phase, lastHB, note, checkpoint, progressAt sql.NullString
	row := s.queryRow(ctx, `SELECT running_turns, input_tokens, output_tokens, last_event_at,
		hb_state, phase, last_heartbeat_at, progress_note, checkpoint, progress_at
		FROM attempt_progress WHERE attempt_id = ?`, attemptID)
	if err := row.Scan(&p.RunningTurns, &p.TokensIn, &p.TokensOut, &lastEvent,
		&hbState, &phase, &lastHB, &note, &checkpoint, &progressAt); err != nil {
		if err == sql.ErrNoRows {
			return nil, nil
		}
		return nil, fmt.Errorf("read progress for %s: %w", attemptID, err)
	}
	var err error
	if p.LastEventAt, err = parseTime(lastEvent); err != nil {
		return nil, err
	}
	if p.PhaseAt, err = parseTime(lastHB); err != nil {
		return nil, err
	}
	if p.NoteAt, err = parseTime(progressAt); err != nil {
		return nil, err
	}
	p.State, p.Phase, p.Note, p.Checkpoint = model.State(hbState.String), phase.String, note.String, checkpoint.String
	return &p, nil
}

// nullSQLString passes a nullable string straight through to a driver arg,
// storing SQL NULL when the source column was NULL.
func nullSQLString(s sql.NullString) any {
	if !s.Valid {
		return nil
	}
	return s.String
}
