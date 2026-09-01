package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"forge/internal/core/protocol"
)

// InsertEvents stores a batch with one prepared statement, ignoring duplicates
// (delivery is at-least-once; (attempt, source, seq) is the idempotency key).
// It returns how many rows were new.
func (tx *Tx) InsertEvents(ctx context.Context, attemptID, source string, events []protocol.Event) (inserted int, err error) {
	if len(events) == 0 {
		return 0, nil
	}
	stmt, err := tx.Prepare(ctx, `INSERT OR IGNORE INTO events (attempt_id, source, seq, time, elapsed_us, kind, message, span_id, parent_id, name, duration_us, attrs) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`)
	if err != nil {
		return 0, fmt.Errorf("prepare events insert: %w", err)
	}
	defer func() { err = errors.Join(err, stmt.Close()) }()
	for _, e := range events {
		if err := e.Validate(); err != nil {
			return inserted, err
		}
		res, err := stmt.ExecContext(ctx, attemptID, source, e.Seq, formatTime(e.Time), e.ElapsedUS, e.Kind, e.Message, nullString(e.SpanID), nullString(e.ParentID), nullString(e.Name), nullInt64(e.DurationUS), jsonRaw(e.Attrs))
		if err != nil {
			return inserted, fmt.Errorf("insert event %d: %w", e.Seq, err)
		}
		n, err := res.RowsAffected()
		if err != nil {
			return inserted, fmt.Errorf("event %d rows affected: %w", e.Seq, err)
		}
		inserted += int(n)
	}
	return inserted, nil
}

func nullInt64(n int64) any {
	if n == 0 {
		return nil
	}
	return n
}

// StoredEvent is an event with its source.
type StoredEvent struct {
	Source string `json:"source"`
	protocol.Event
}

// Events returns an attempt's events ordered by elapsed time then source and
// seq, optionally excluding stdout/stderr lines (the retro pack wants spans).
func (s *Store) Events(ctx context.Context, attemptID string, includeLines bool, limit int) ([]StoredEvent, error) {
	q := `SELECT source, seq, time, elapsed_us, kind, message, span_id, parent_id, name, duration_us, attrs FROM events WHERE attempt_id = ?`
	if !includeLines {
		q += ` AND kind NOT IN ('stdout','stderr')`
	}
	q += ` ORDER BY elapsed_us, source, seq`
	args := []any{attemptID}
	if limit > 0 {
		q += ` LIMIT ?`
		args = append(args, limit)
	}
	var out []StoredEvent
	err := each(s.query(ctx, q, args...))(func(rows *sql.Rows) error {
		var e StoredEvent
		var ts string
		var span, parent, name, attrs sql.NullString
		var dur sql.NullInt64
		if err := rows.Scan(&e.Source, &e.Seq, &ts, &e.ElapsedUS, &e.Kind, &e.Message, &span, &parent, &name, &dur, &attrs); err != nil {
			return fmt.Errorf("scan event: %w", err)
		}
		t, err := time.Parse(time.RFC3339Nano, ts)
		if err != nil {
			return fmt.Errorf("event %d: %w", e.Seq, err)
		}
		e.Time, e.SpanID, e.ParentID, e.Name, e.DurationUS = t, span.String, parent.String, name.String, dur.Int64
		if attrs.Valid {
			e.Attrs = json.RawMessage(attrs.String)
		}
		out = append(out, e)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read events of %s: %w", attemptID, err)
	}
	return out, nil
}

// LineEventStats returns how many stdout/stderr events and bytes an attempt has
// stored — the cap's input.
func (s *Store) LineEventStats(ctx context.Context, attemptID string) (count int, bytes int64, err error) {
	err = s.queryRow(ctx, `SELECT count(*), COALESCE(sum(length(message)), 0) FROM events WHERE attempt_id = ? AND kind IN ('stdout','stderr')`, attemptID).Scan(&count, &bytes)
	if err != nil {
		return 0, 0, fmt.Errorf("count line events of %s: %w", attemptID, err)
	}
	return count, bytes, nil
}
