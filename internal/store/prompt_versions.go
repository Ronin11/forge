package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"

	"forge/internal/protocol"
)

// UpsertPromptVersion records a prompt configuration once; the hash is the key.
func (tx *Tx) UpsertPromptVersion(ctx context.Context, pv protocol.PromptVersion) error {
	tools, err := json.Marshal(pv.ToolList)
	if err != nil {
		return fmt.Errorf("marshal tool list: %w", err)
	}
	if _, err := tx.Exec(ctx, `INSERT OR IGNORE INTO prompt_versions (hash, routine, generation, mode, template, rendered_example, system_append, tool_list, model, effort, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		pv.Hash, pv.Routine, pv.Generation, pv.Mode, pv.Template, pv.RenderedExample, pv.SystemAppend, string(tools), pv.Model, nullString(pv.Effort), formatTime(tx.now)); err != nil {
		return fmt.Errorf("upsert prompt version %s: %w", pv.Hash, err)
	}
	return nil
}

// GetPromptVersion reads one by hash.
func (s *Store) GetPromptVersion(ctx context.Context, hash string) (*protocol.PromptVersion, error) {
	var pv protocol.PromptVersion
	var tools string
	var effort sql.NullString
	err := s.queryRow(ctx, `SELECT hash, routine, generation, mode, template, rendered_example, system_append, tool_list, model, effort FROM prompt_versions WHERE hash = ?`, hash).
		Scan(&pv.Hash, &pv.Routine, &pv.Generation, &pv.Mode, &pv.Template, &pv.RenderedExample, &pv.SystemAppend, &tools, &pv.Model, &effort)
	if isNoRows(err) {
		return nil, fmt.Errorf("prompt version %s: %w", hash, ErrNotFound)
	}
	if err != nil {
		return nil, fmt.Errorf("read prompt version %s: %w", hash, err)
	}
	pv.Effort = effort.String
	if err := json.Unmarshal([]byte(tools), &pv.ToolList); err != nil {
		return nil, fmt.Errorf("decode tool list of %s: %w", hash, err)
	}
	return &pv, nil
}
