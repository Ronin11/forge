package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"
)

// Entity types and kinds are strings so plugins can read the journal without a
// Go dependency; the constants keep Forge's own spelling in one place.
const (
	EntityWork     = "work"
	EntityTarget   = "target"
	EntityAttempt  = "attempt"
	EntityQuestion = "question"
	EntityProposal = "proposal"
	EntityDaemon   = "daemon"
	EntityPlugin   = "plugin"
	EntityMerge    = "merge"
	EntityWorkflow = "workflow"
)

// JournalEntry is one row of the audit trail.
type JournalEntry struct {
	ID         int64           `json:"id"`
	Time       time.Time       `json:"ts"`
	Kind       string          `json:"kind"`
	EntityType string          `json:"entity_type"`
	EntityID   string          `json:"entity_id"`
	Payload    json.RawMessage `json:"payload"`
}

// Journal writes one audit row in this transaction. Every state-changing store
// method calls it; a change without a journal row is a bug (STYLE.md §9).
func (tx *Tx) Journal(ctx context.Context, kind, entityType, entityID string, payload any) error {
	body, err := json.Marshal(payload)
	if err != nil {
		return fmt.Errorf("marshal journal payload for %s: %w", kind, err)
	}
	if _, err := tx.Exec(ctx, `INSERT INTO journal (ts, kind, entity_type, entity_id, payload) VALUES (?, ?, ?, ?, ?)`,
		formatTime(tx.now), kind, entityType, entityID, string(body)); err != nil {
		return fmt.Errorf("journal %s: %w", kind, err)
	}
	return nil
}

// JournalSince returns up to limit rows with id > since, oldest first — the
// cursor contract plugins and the SSE stream rely on.
func (s *Store) JournalSince(ctx context.Context, since int64, limit int) ([]JournalEntry, error) {
	if limit <= 0 || limit > 1000 {
		limit = 1000
	}
	return s.journalRows(each(s.query(ctx, `SELECT id, ts, kind, entity_type, entity_id, payload FROM journal WHERE id > ? ORDER BY id LIMIT ?`, since, limit)))
}

// PluginDenialRow is one plugin's recent scope-denial tally for the doctor.
type PluginDenialRow struct {
	Plugin   string
	Count    int
	LastPath string
}

// PluginDenialsSince tallies plugin.denied journal rows per plugin after the
// cutoff, with the most recently denied path as the sample.
func (s *Store) PluginDenialsSince(ctx context.Context, since time.Time) ([]PluginDenialRow, error) {
	rows, err := s.query(ctx, `
		SELECT entity_id, COUNT(*),
		       COALESCE((SELECT json_extract(j2.payload, '$.path') FROM journal j2
		         WHERE j2.kind = 'plugin.denied' AND j2.entity_id = j.entity_id
		         ORDER BY j2.id DESC LIMIT 1), '')
		FROM journal j WHERE j.kind = 'plugin.denied' AND j.ts > ?
		GROUP BY entity_id ORDER BY entity_id`, formatTime(since))
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []PluginDenialRow
	for rows.Next() {
		var r PluginDenialRow
		if err := rows.Scan(&r.Plugin, &r.Count, &r.LastPath); err != nil {
			return nil, err
		}
		out = append(out, r)
	}
	return out, rows.Err()
}

// JournalForEntity returns an entity's history, oldest first.
func (s *Store) JournalForEntity(ctx context.Context, entityType, entityID string) ([]JournalEntry, error) {
	return s.journalRows(each(s.query(ctx, `SELECT id, ts, kind, entity_type, entity_id, payload FROM journal WHERE entity_type = ? AND entity_id = ? ORDER BY id`, entityType, entityID)))
}

func (s *Store) journalRows(iter func(func(*sql.Rows) error) error) ([]JournalEntry, error) {
	var out []JournalEntry
	err := iter(func(rows *sql.Rows) error {
		var e JournalEntry
		var ts, payload string
		if err := rows.Scan(&e.ID, &ts, &e.Kind, &e.EntityType, &e.EntityID, &payload); err != nil {
			return fmt.Errorf("scan journal: %w", err)
		}
		t, err := time.Parse(time.RFC3339Nano, ts)
		if err != nil {
			return fmt.Errorf("journal %d: %w", e.ID, err)
		}
		e.Time, e.Payload = t, json.RawMessage(payload)
		out = append(out, e)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read journal: %w", err)
	}
	return out, nil
}
