package store

// Read methods behind GET /api/v1/work/{id}/stream (DESIGN.md §13): the
// stream polls these with cursors, so both are strictly ordered and bounded.

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	"forge/internal/protocol"
)

// JournalForWorkSince returns the journal rows that tell one Work's story —
// the Work itself, its Targets, their Attempts, and its Questions — with
// id > since, oldest first. The journal id is the stream's resume cursor:
// ids are assigned in commit order, so a client that saw id N misses nothing
// by asking for > N.
func (s *Store) JournalForWorkSince(ctx context.Context, workID string, since int64, limit int) ([]JournalEntry, error) {
	if limit <= 0 || limit > 1000 {
		limit = 1000
	}
	return s.journalRows(each(s.query(ctx, `SELECT id, ts, kind, entity_type, entity_id, payload FROM journal
		WHERE id > ? AND (
			(entity_type = ? AND entity_id = ?)
			OR (entity_type = ? AND entity_id IN (SELECT id FROM targets WHERE work_id = ?))
			OR (entity_type = ? AND entity_id IN (SELECT a.id FROM attempts a JOIN targets t ON a.target_id = t.id WHERE t.work_id = ?))
			OR (entity_type = ? AND entity_id IN (SELECT id FROM questions WHERE work_id = ?))
		) ORDER BY id LIMIT ?`,
		since, EntityWork, workID, EntityTarget, workID, EntityAttempt, workID, EntityQuestion, workID, limit)))
}

// EventSeqs is a resumable cursor over one attempt's events. The events table
// has no rowid (its key is attempt, source, seq), but seq is monotonic within
// (attempt, source), so one floor per source names a position exactly. Each
// floor is the next seq wanted, so the zero value reads from the beginning.
type EventSeqs struct {
	Worker  int
	MCP     int
	Control int
}

// Advance moves the floor for e's source past it.
func (c *EventSeqs) Advance(e StoredEvent) {
	switch e.Source {
	case protocol.SourceWorker:
		if e.Seq >= c.Worker {
			c.Worker = e.Seq + 1
		}
	case protocol.SourceMCP:
		if e.Seq >= c.MCP {
			c.MCP = e.Seq + 1
		}
	case protocol.SourceControl:
		if e.Seq >= c.Control {
			c.Control = e.Seq + 1
		}
	}
}

// EventsSinceSeq returns an attempt's events at or beyond the per-source
// floors, ordered by source then seq — the order that makes floor-based
// pagination lossless even when a batch is cut by limit.
func (s *Store) EventsSinceSeq(ctx context.Context, attemptID string, after EventSeqs, limit int) ([]StoredEvent, error) {
	if limit <= 0 || limit > 1000 {
		limit = 1000
	}
	var out []StoredEvent
	err := each(s.query(ctx, `SELECT source, seq, time, elapsed_us, kind, message, span_id, parent_id, name, duration_us, attrs FROM events
		WHERE attempt_id = ? AND ((source = ? AND seq >= ?) OR (source = ? AND seq >= ?) OR (source = ? AND seq >= ?))
		ORDER BY source, seq LIMIT ?`,
		attemptID, protocol.SourceWorker, after.Worker, protocol.SourceMCP, after.MCP, protocol.SourceControl, after.Control, limit))(func(rows *sql.Rows) error {
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
		return nil, fmt.Errorf("read events of %s since %+v: %w", attemptID, after, err)
	}
	return out, nil
}
