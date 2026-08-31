package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
)

// ExternalRef is one link a plugin (or an operator) attaches to a Work
// (DESIGN.md §17): the UI renders them generically.
type ExternalRef struct {
	Plugin string `json:"plugin"`
	Kind   string `json:"kind"`
	ID     string `json:"id"`
	URL    string `json:"url,omitempty"`
	Label  string `json:"label,omitempty"`
}

// AddWorkExternalRefs appends refs to the Work's external_refs, deduplicating
// by (plugin, kind, id) so an annotating plugin never clobbers another's
// links, and journals the annotation.
func (tx *Tx) AddWorkExternalRefs(ctx context.Context, id string, refs []ExternalRef) error {
	var raw sql.NullString
	if err := tx.QueryRow(ctx, `SELECT external_refs FROM work WHERE id = ?`, id).Scan(&raw); err != nil {
		if err == sql.ErrNoRows {
			return fmt.Errorf("work %s: %w", id, ErrNotFound)
		}
		return fmt.Errorf("read work %s external refs: %w", id, err)
	}
	var existing []ExternalRef
	if raw.Valid && raw.String != "" {
		if err := json.Unmarshal([]byte(raw.String), &existing); err != nil {
			return fmt.Errorf("work %s external refs: %w", id, err)
		}
	}
	seen := map[string]bool{}
	for _, r := range existing {
		seen[r.Plugin+"\x00"+r.Kind+"\x00"+r.ID] = true
	}
	added := 0
	for _, r := range refs {
		key := r.Plugin + "\x00" + r.Kind + "\x00" + r.ID
		if seen[key] {
			continue
		}
		seen[key] = true
		existing = append(existing, r)
		added++
	}
	if added == 0 {
		return nil
	}
	merged, err := json.Marshal(existing)
	if err != nil {
		return fmt.Errorf("marshal external refs: %w", err)
	}
	if _, err := tx.Exec(ctx, `UPDATE work SET external_refs = ? WHERE id = ?`, string(merged), id); err != nil {
		return fmt.Errorf("annotate work %s: %w", id, err)
	}
	return tx.Journal(ctx, "work.annotated", EntityWork, id, map[string]any{"added": added, "refs": refs})
}
