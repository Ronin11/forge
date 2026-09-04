package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	"forge/internal/core/model"
)

// Experiment is one LLM-assisted optimization run over a subject
// (<kind>:<name> — persona and routine today, workflow by design next): the
// optimizer model proposes variants, each runs the operator's test on the
// target model, and the optimizer judges the outputs against the goal. Test
// is a JSON spec whose shape belongs to the subject kind. The row is the
// experiment's whole life — the background worker updates progress and
// finally writes results; nothing applies automatically.
type Experiment struct {
	ID             string          `json:"id"`
	Subject        string          `json:"subject"`
	Goal           string          `json:"goal"`
	TargetModel    string          `json:"target_model"`
	OptimizerModel string          `json:"optimizer_model"`
	Test           json.RawMessage `json:"test,omitempty"`
	VariantCount   int             `json:"variant_count"`
	Status         string          `json:"status"`
	Progress       string          `json:"progress,omitempty"`
	Baseline       string          `json:"baseline,omitempty"`
	Results        json.RawMessage `json:"results,omitempty"`
	Error          string          `json:"error,omitempty"`
	CreatedAt      time.Time       `json:"created_at"`
	UpdatedAt      time.Time       `json:"updated_at"`
}

// Experiment statuses.
const (
	ExperimentRunning = "running"
	ExperimentDone    = "done"
	ExperimentFailed  = "failed"
)

// experimentKeep bounds retained experiments per subject.
const experimentKeep = 5

// InsertExperiment records a new experiment (status running) and trims the
// subject's history.
func (tx *Tx) InsertExperiment(ctx context.Context, pe *Experiment) error {
	if pe.ID == "" {
		pe.ID = model.NewID()
	}
	pe.Status = ExperimentRunning
	pe.CreatedAt, pe.UpdatedAt = tx.now, tx.now
	_, err := tx.Exec(ctx, `INSERT INTO experiments (id, subject, goal, target_model, optimizer_model, test, variant_count, status, progress, baseline, results, error, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		pe.ID, pe.Subject, pe.Goal, pe.TargetModel, pe.OptimizerModel, jsonRaw(pe.Test), pe.VariantCount, pe.Status, nullString(pe.Progress), nullString(pe.Baseline), jsonRaw(pe.Results), nullString(pe.Error), formatTime(pe.CreatedAt), formatTime(pe.UpdatedAt))
	if err != nil {
		return fmt.Errorf("insert experiment: %w", err)
	}
	if _, err := tx.Exec(ctx, `DELETE FROM experiments WHERE subject = ? AND id NOT IN (SELECT id FROM experiments WHERE subject = ? ORDER BY created_at DESC, id DESC LIMIT ?)`,
		pe.Subject, pe.Subject, experimentKeep); err != nil {
		return fmt.Errorf("trim experiments: %w", err)
	}
	return tx.Journal(ctx, "experiment.started", EntityDaemon, pe.ID, map[string]any{"subject": pe.Subject, "goal": pe.Goal, "target": pe.TargetModel, "optimizer": pe.OptimizerModel, "variants": pe.VariantCount})
}

// SetExperimentProgress updates the running line.
func (tx *Tx) SetExperimentProgress(ctx context.Context, id, progress string) error {
	res, err := tx.Exec(ctx, `UPDATE experiments SET progress = ?, updated_at = ? WHERE id = ? AND status = ?`, progress, formatTime(tx.now), id, ExperimentRunning)
	if err != nil {
		return fmt.Errorf("set experiment progress: %w", err)
	}
	return oneRow(res, "experiment "+id)
}

// FinishExperiment writes the outcome: done with results, or failed with an
// error.
func (tx *Tx) FinishExperiment(ctx context.Context, id, status string, results json.RawMessage, errMsg string) error {
	res, err := tx.Exec(ctx, `UPDATE experiments SET status = ?, results = ?, error = ?, progress = NULL, updated_at = ? WHERE id = ? AND status = ?`,
		status, jsonRaw(results), nullString(errMsg), formatTime(tx.now), id, ExperimentRunning)
	if err != nil {
		return fmt.Errorf("finish experiment %s: %w", id, err)
	}
	if err := oneRow(res, "experiment "+id); err != nil {
		return err
	}
	return tx.Journal(ctx, "experiment.finished", EntityDaemon, id, map[string]any{"status": status, "error": errMsg})
}

const experimentColumns = `id, subject, goal, target_model, optimizer_model, test, variant_count, status, progress, baseline, results, error, created_at, updated_at`

// GetExperiment reads one experiment.
func (s *Store) GetExperiment(ctx context.Context, id string) (*Experiment, error) {
	list, err := scanExperiments(each(s.query(ctx, `SELECT `+experimentColumns+` FROM experiments WHERE id = ?`, id)))
	if err != nil {
		return nil, err
	}
	if len(list) == 0 {
		return nil, fmt.Errorf("experiment %s: %w", id, ErrNotFound)
	}
	return &list[0], nil
}

// Experiments lists a subject's experiments, newest first.
func (s *Store) Experiments(ctx context.Context, subject string, limit int) ([]Experiment, error) {
	if limit <= 0 || limit > experimentKeep {
		limit = experimentKeep
	}
	return scanExperiments(each(s.query(ctx, `SELECT `+experimentColumns+` FROM experiments WHERE subject = ? ORDER BY created_at DESC, id DESC LIMIT ?`, subject, limit)))
}

func scanExperiments(iter func(func(*sql.Rows) error) error) ([]Experiment, error) {
	var out []Experiment
	err := iter(func(rows *sql.Rows) error {
		var pe Experiment
		var test, progress, baseline, results, errMsg sql.NullString
		var created, updated string
		if err := rows.Scan(&pe.ID, &pe.Subject, &pe.Goal, &pe.TargetModel, &pe.OptimizerModel, &test, &pe.VariantCount, &pe.Status, &progress, &baseline, &results, &errMsg, &created, &updated); err != nil {
			return fmt.Errorf("scan experiment: %w", err)
		}
		pe.Progress, pe.Baseline, pe.Error = progress.String, baseline.String, errMsg.String
		if test.Valid && test.String != "" {
			pe.Test = json.RawMessage(test.String)
		}
		if results.Valid && results.String != "" {
			pe.Results = json.RawMessage(results.String)
		}
		var err error
		if pe.CreatedAt, err = parseTime(sql.NullString{String: created, Valid: true}); err != nil {
			return err
		}
		if pe.UpdatedAt, err = parseTime(sql.NullString{String: updated, Valid: true}); err != nil {
			return err
		}
		out = append(out, pe)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read experiments: %w", err)
	}
	return out, nil
}
