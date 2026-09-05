package store

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"time"
)

// ScratchScript is one cached agent-authored script: the organic layer under
// the curated library. Rows live and die by use (LRU) unless promotion moves
// them into git.
type ScratchScript struct {
	Name        string    `json:"name"`
	Source      string    `json:"source"`
	Hash        string    `json:"hash"`
	Language    string    `json:"language"`
	Description string    `json:"description"`
	InputSchema string    `json:"input_schema,omitempty"`
	CreatedBy   string    `json:"created_by"`
	RunCount    int       `json:"run_count"`
	Attempts    []string  `json:"attempts,omitempty"`
	CreatedAt   time.Time `json:"created_at"`
	LastRunAt   time.Time `json:"last_run_at"`
}

// scratchAttemptsKept bounds the distinct-attempt list a row remembers.
const scratchAttemptsKept = 20

// UpsertScratch saves a scratch script. Same name + same source is a no-op
// (the run touch happens separately); a new source replaces the row and
// resets its counters — it is a different script wearing the same name.
// keep bounds the cache: the least-recently-used rows beyond it are evicted.
func (tx *Tx) UpsertScratch(ctx context.Context, s *ScratchScript, keep int) error {
	sum := sha256.Sum256([]byte(s.Source))
	s.Hash = hex.EncodeToString(sum[:])
	var existingHash string
	err := tx.QueryRow(ctx, `SELECT hash FROM scratch_scripts WHERE name = ?`, s.Name).Scan(&existingHash)
	switch {
	case err == nil && existingHash == s.Hash:
		return nil // unchanged; the caller's touch records the run
	case err == nil:
		s.CreatedAt, s.LastRunAt, s.RunCount, s.Attempts = tx.now, tx.now, 0, nil
		_, err = tx.Exec(ctx, `UPDATE scratch_scripts SET source=?, hash=?, language=?, description=?, input_schema=?, created_by=?, run_count=0, attempts='[]', created_at=?, last_run_at=? WHERE name=?`,
			s.Source, s.Hash, s.Language, s.Description, s.InputSchema, s.CreatedBy, formatTime(s.CreatedAt), formatTime(s.LastRunAt), s.Name)
		if err != nil {
			return fmt.Errorf("replace scratch %s: %w", s.Name, err)
		}
	default:
		s.CreatedAt, s.LastRunAt = tx.now, tx.now
		_, err = tx.Exec(ctx, `INSERT INTO scratch_scripts (name, source, hash, language, description, input_schema, created_by, run_count, attempts, created_at, last_run_at) VALUES (?, ?, ?, ?, ?, ?, ?, 0, '[]', ?, ?)`,
			s.Name, s.Source, s.Hash, s.Language, s.Description, s.InputSchema, s.CreatedBy, formatTime(s.CreatedAt), formatTime(s.LastRunAt))
		if err != nil {
			return fmt.Errorf("insert scratch %s: %w", s.Name, err)
		}
	}
	if keep > 0 {
		if _, err := tx.Exec(ctx, `DELETE FROM scratch_scripts WHERE name NOT IN (SELECT name FROM scratch_scripts ORDER BY last_run_at DESC, name LIMIT ?)`, keep); err != nil {
			return fmt.Errorf("evict scratch: %w", err)
		}
	}
	return nil
}

// TouchScratchRun records one execution: bumps the counter, remembers the
// calling attempt (bounded, distinct), refreshes recency, and returns the
// updated row so the caller can check the promotion threshold.
func (tx *Tx) TouchScratchRun(ctx context.Context, name, attemptID string) (*ScratchScript, error) {
	s, err := scratchRow(tx.QueryRow(ctx, `SELECT `+scratchColumns+` FROM scratch_scripts WHERE name = ?`, name))
	if err != nil {
		return nil, err
	}
	seen := false
	for _, a := range s.Attempts {
		if a == attemptID {
			seen = true
			break
		}
	}
	if !seen && attemptID != "" && len(s.Attempts) < scratchAttemptsKept {
		s.Attempts = append(s.Attempts, attemptID)
	}
	s.RunCount++
	s.LastRunAt = tx.now
	attempts, err := json.Marshal(s.Attempts)
	if err != nil {
		return nil, err
	}
	if _, err := tx.Exec(ctx, `UPDATE scratch_scripts SET run_count=?, attempts=?, last_run_at=? WHERE name=?`,
		s.RunCount, string(attempts), formatTime(s.LastRunAt), name); err != nil {
		return nil, fmt.Errorf("touch scratch %s: %w", name, err)
	}
	return s, nil
}

// GetScratch reads one row by name.
func (tx *Tx) GetScratch(ctx context.Context, name string) (*ScratchScript, error) {
	return scratchRow(tx.QueryRow(ctx, `SELECT `+scratchColumns+` FROM scratch_scripts WHERE name = ?`, name))
}

// DeleteScratch removes one row (promotion's last step).
func (tx *Tx) DeleteScratch(ctx context.Context, name string) error {
	if _, err := tx.Exec(ctx, `DELETE FROM scratch_scripts WHERE name = ?`, name); err != nil {
		return fmt.Errorf("delete scratch %s: %w", name, err)
	}
	return nil
}

// ListScratch returns every cached script, most recently used first — the
// search layer and the UI section read this.
func (s *Store) ListScratch(ctx context.Context) ([]ScratchScript, error) {
	var out []ScratchScript
	err := each(s.query(ctx, `SELECT `+scratchColumns+` FROM scratch_scripts ORDER BY last_run_at DESC, name`))(func(rows *sql.Rows) error {
		sc, err := scanScratch(rows.Scan)
		if err != nil {
			return err
		}
		out = append(out, *sc)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("list scratch: %w", err)
	}
	return out, nil
}

const scratchColumns = `name, source, hash, language, description, input_schema, created_by, run_count, attempts, created_at, last_run_at`

func scratchRow(row *sql.Row) (*ScratchScript, error) {
	s, err := scanScratch(row.Scan)
	if isNoRows(err) {
		return nil, fmt.Errorf("scratch script: %w", ErrNotFound)
	}
	return s, err
}

func scanScratch(scan func(...any) error) (*ScratchScript, error) {
	var s ScratchScript
	var attempts, created, lastRun string
	if err := scan(&s.Name, &s.Source, &s.Hash, &s.Language, &s.Description, &s.InputSchema, &s.CreatedBy, &s.RunCount, &attempts, &created, &lastRun); err != nil {
		return nil, err
	}
	if err := json.Unmarshal([]byte(attempts), &s.Attempts); err != nil {
		return nil, fmt.Errorf("decode scratch attempts: %w", err)
	}
	var err error
	if s.CreatedAt, err = parseTime(sql.NullString{String: created, Valid: true}); err != nil {
		return nil, err
	}
	if s.LastRunAt, err = parseTime(sql.NullString{String: lastRun, Valid: true}); err != nil {
		return nil, err
	}
	return &s, nil
}
