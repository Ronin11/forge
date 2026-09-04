package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	"forge/internal/core/model"
)

// PromptTest is one recorded run of the Prompts page's tester: the inputs,
// the composed prompt and its manifest, and the model's output — what the
// page shows when you come back to a prompt to iterate on it.
type PromptTest struct {
	ID          string          `json:"id"`
	Subject     string          `json:"subject"` // persona:<name> | routine:<name>
	Persona     string          `json:"persona,omitempty"`
	Routine     string          `json:"routine,omitempty"`
	Mode        string          `json:"mode,omitempty"`
	Task        string          `json:"task,omitempty"`
	Objective   string          `json:"objective,omitempty"`
	Repo        string          `json:"repo,omitempty"`
	Model       string          `json:"model"`
	Prompt      string          `json:"prompt,omitempty"`
	Composition json.RawMessage `json:"composition,omitempty"`
	Output      string          `json:"output,omitempty"`
	ElapsedMS   int64           `json:"elapsed_ms"`
	CreatedAt   time.Time       `json:"created_at"`
}

// promptTestKeep is how many runs each subject retains — a scratchpad for
// iteration, not an archive.
const promptTestKeep = 20

// InsertPromptTest records one run and trims the subject to its most recent
// promptTestKeep. Prompt and output are capped at MaxNodeOutputBytes.
func (tx *Tx) InsertPromptTest(ctx context.Context, pt *PromptTest) error {
	if pt.ID == "" {
		pt.ID = model.NewID()
	}
	pt.CreatedAt = tx.now
	pt.Prompt = cutBytesStore(pt.Prompt, MaxNodeOutputBytes)
	pt.Output = cutBytesStore(pt.Output, MaxNodeOutputBytes)
	_, err := tx.Exec(ctx, `INSERT INTO prompt_tests (id, subject, persona, routine, mode, task, objective, repo, model, prompt, composition, output, elapsed_ms, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		pt.ID, pt.Subject, nullString(pt.Persona), nullString(pt.Routine), nullString(pt.Mode), nullString(pt.Task), nullString(pt.Objective), nullString(pt.Repo), pt.Model, nullString(pt.Prompt), jsonRaw(pt.Composition), nullString(pt.Output), pt.ElapsedMS, formatTime(pt.CreatedAt))
	if err != nil {
		return fmt.Errorf("insert prompt test: %w", err)
	}
	if _, err := tx.Exec(ctx, `DELETE FROM prompt_tests WHERE subject = ? AND id NOT IN (SELECT id FROM prompt_tests WHERE subject = ? ORDER BY created_at DESC, id DESC LIMIT ?)`,
		pt.Subject, pt.Subject, promptTestKeep); err != nil {
		return fmt.Errorf("trim prompt tests: %w", err)
	}
	return tx.Journal(ctx, "prompt.test_run", EntityDaemon, pt.ID, map[string]any{"subject": pt.Subject, "model": pt.Model, "elapsed_ms": pt.ElapsedMS})
}

// PromptTests lists a subject's recorded runs, newest first.
func (s *Store) PromptTests(ctx context.Context, subject string, limit int) ([]PromptTest, error) {
	if limit <= 0 || limit > promptTestKeep {
		limit = promptTestKeep
	}
	var out []PromptTest
	err := each(s.query(ctx, `SELECT id, subject, persona, routine, mode, task, objective, repo, model, prompt, composition, output, elapsed_ms, created_at FROM prompt_tests WHERE subject = ? ORDER BY created_at DESC, id DESC LIMIT ?`, subject, limit))(func(rows *sql.Rows) error {
		var pt PromptTest
		var persona, routine, mode, task, objective, repo, prompt, composition, output sql.NullString
		var created string
		if err := rows.Scan(&pt.ID, &pt.Subject, &persona, &routine, &mode, &task, &objective, &repo, &pt.Model, &prompt, &composition, &output, &pt.ElapsedMS, &created); err != nil {
			return fmt.Errorf("scan prompt test: %w", err)
		}
		pt.Persona, pt.Routine, pt.Mode, pt.Task = persona.String, routine.String, mode.String, task.String
		pt.Objective, pt.Repo, pt.Prompt, pt.Output = objective.String, repo.String, prompt.String, output.String
		if composition.Valid && composition.String != "" {
			pt.Composition = json.RawMessage(composition.String)
		}
		var err error
		if pt.CreatedAt, err = parseTime(sql.NullString{String: created, Valid: true}); err != nil {
			return err
		}
		out = append(out, pt)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read prompt tests: %w", err)
	}
	return out, nil
}

// cutBytesStore trims s to at most n bytes at a rune boundary.
func cutBytesStore(s string, n int) string {
	if len(s) <= n {
		return s
	}
	for n > 0 && (s[n]&0xC0) == 0x80 {
		n--
	}
	return s[:n]
}
