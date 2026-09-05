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
	// Live-experiment fields (Kind "live"): Arms is the pinned arm set
	// (arms[0] = control, its Hash the library fragment's hash at go-live);
	// MinRuns the per-arm decision window; DecideBy the inconclusive
	// deadline.
	Kind     string          `json:"kind,omitempty"`
	Arms     json.RawMessage `json:"arms,omitempty"`
	MinRuns  int             `json:"min_runs,omitempty"`
	OpenedAt time.Time       `json:"opened_at,omitempty"`
	DecideBy time.Time       `json:"decide_by,omitempty"`

	CreatedAt time.Time `json:"created_at"`
	UpdatedAt time.Time `json:"updated_at"`
}

// ExperimentArm is one pinned arm of a live experiment.
type ExperimentArm struct {
	Label   string `json:"label"` // "control", "v1", …
	Title   string `json:"title,omitempty"`
	Content string `json:"content"`
	Hash    string `json:"hash"`
}

// Experiment kinds and statuses. Offline: running → done|failed. Live:
// running (arm setup) → live (assigning) → promoted|kept_control|
// inconclusive|aborted|failed.
const (
	ExperimentKindOffline = "offline"
	ExperimentKindLive    = "live"

	ExperimentRunning      = "running"
	ExperimentDone         = "done"
	ExperimentFailed       = "failed"
	ExperimentLive         = "live"
	ExperimentPromoted     = "promoted"
	ExperimentKeptControl  = "kept_control"
	ExperimentInconclusive = "inconclusive"
	ExperimentAborted      = "aborted"
)

// experimentKeep bounds retained experiments per subject.
const experimentKeep = 5

// InsertExperiment records a new experiment (status running) and trims the
// subject's history.
func (tx *Tx) InsertExperiment(ctx context.Context, pe *Experiment) error {
	if pe.ID == "" {
		pe.ID = model.NewID()
	}
	if pe.Kind == "" {
		pe.Kind = ExperimentKindOffline
	}
	pe.Status = ExperimentRunning
	pe.CreatedAt, pe.UpdatedAt = tx.now, tx.now
	_, err := tx.Exec(ctx, `INSERT INTO experiments (id, subject, goal, target_model, optimizer_model, test, variant_count, status, progress, baseline, results, error, kind, arms, min_runs, opened_at, decide_by, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		pe.ID, pe.Subject, pe.Goal, pe.TargetModel, pe.OptimizerModel, jsonRaw(pe.Test), pe.VariantCount, pe.Status, nullString(pe.Progress), nullString(pe.Baseline), jsonRaw(pe.Results), nullString(pe.Error),
		pe.Kind, jsonRaw(pe.Arms), pe.MinRuns, nullTime(pe.OpenedAt), nullTime(pe.DecideBy), formatTime(pe.CreatedAt), formatTime(pe.UpdatedAt))
	if err != nil {
		if isUniqueViolation(err) {
			return fmt.Errorf("a live experiment is already open for %s: %w", pe.Subject, ErrConflict)
		}
		return fmt.Errorf("insert experiment: %w", err)
	}
	// Trim history — but never a non-terminal row: a live experiment must not
	// be deleted out from under its assignment cache by later offline runs.
	if _, err := tx.Exec(ctx, `DELETE FROM experiments WHERE subject = ? AND status NOT IN (?, ?) AND id NOT IN (SELECT id FROM experiments WHERE subject = ? AND status NOT IN (?, ?) ORDER BY created_at DESC, id DESC LIMIT ?)`,
		pe.Subject, ExperimentRunning, ExperimentLive, pe.Subject, ExperimentRunning, ExperimentLive, experimentKeep); err != nil {
		return fmt.Errorf("trim experiments: %w", err)
	}
	return tx.Journal(ctx, "experiment.started", EntityDaemon, pe.ID, map[string]any{"subject": pe.Subject, "goal": pe.Goal, "target": pe.TargetModel, "optimizer": pe.OptimizerModel, "variants": pe.VariantCount, "kind": pe.Kind})
}

// SetExperimentLive moves a live-kind setup row into assignment: the arm set
// is pinned (arms[0] = control, read from the CURRENT library at this
// moment, not create time — generation takes minutes and the file may move).
func (tx *Tx) SetExperimentLive(ctx context.Context, id, baseline string, arms json.RawMessage, minRuns int, decideBy time.Time) error {
	res, err := tx.Exec(ctx, `UPDATE experiments SET status = ?, baseline = ?, arms = ?, min_runs = ?, opened_at = ?, decide_by = ?, progress = NULL, updated_at = ? WHERE id = ? AND kind = ? AND status = ?`,
		ExperimentLive, nullString(baseline), jsonRaw(arms), minRuns, formatTime(tx.now), formatTime(decideBy), formatTime(tx.now), id, ExperimentKindLive, ExperimentRunning)
	if err != nil {
		return fmt.Errorf("set experiment %s live: %w", id, err)
	}
	if err := oneRow(res, "experiment "+id); err != nil {
		return err
	}
	return tx.Journal(ctx, "experiment.live", EntityDaemon, id, map[string]any{"min_runs": minRuns, "decide_by": formatTime(decideBy)})
}

// DecideLiveExperiment closes a live experiment with one of its terminal
// statuses and the per-arm results.
func (tx *Tx) DecideLiveExperiment(ctx context.Context, id, status string, results json.RawMessage, errMsg string) error {
	switch status {
	case ExperimentPromoted, ExperimentKeptControl, ExperimentInconclusive, ExperimentAborted, ExperimentFailed, ExperimentDone:
	default:
		return fmt.Errorf("decide experiment %s: status %q is not terminal", id, status)
	}
	res, err := tx.Exec(ctx, `UPDATE experiments SET status = ?, results = ?, error = ?, progress = NULL, updated_at = ? WHERE id = ? AND kind = ? AND status IN (?, ?)`,
		status, jsonRaw(results), nullString(errMsg), formatTime(tx.now), id, ExperimentKindLive, ExperimentRunning, ExperimentLive)
	if err != nil {
		return fmt.Errorf("decide experiment %s: %w", id, err)
	}
	if err := oneRow(res, "experiment "+id); err != nil {
		return err
	}
	return tx.Journal(ctx, "experiment.finished", EntityDaemon, id, map[string]any{"status": status, "error": errMsg})
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

// LiveExperiments returns every open live-kind row — the assignment cache's
// and the decide pass's input (running included so stale setups can be
// failed, freeing the one-live-per-subject index).
func (s *Store) LiveExperiments(ctx context.Context) ([]Experiment, error) {
	return scanExperiments(each(s.query(ctx, `SELECT `+experimentColumns+` FROM experiments WHERE kind = ? AND status IN (?, ?) ORDER BY created_at`, ExperimentKindLive, ExperimentRunning, ExperimentLive)))
}

const experimentColumns = `id, subject, goal, target_model, optimizer_model, test, variant_count, status, progress, baseline, results, error, kind, arms, min_runs, opened_at, decide_by, created_at, updated_at`

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
		var test, progress, baseline, results, errMsg, arms, opened, decideBy sql.NullString
		var created, updated string
		if err := rows.Scan(&pe.ID, &pe.Subject, &pe.Goal, &pe.TargetModel, &pe.OptimizerModel, &test, &pe.VariantCount, &pe.Status, &progress, &baseline, &results, &errMsg, &pe.Kind, &arms, &pe.MinRuns, &opened, &decideBy, &created, &updated); err != nil {
			return fmt.Errorf("scan experiment: %w", err)
		}
		pe.Progress, pe.Baseline, pe.Error = progress.String, baseline.String, errMsg.String
		if test.Valid && test.String != "" {
			pe.Test = json.RawMessage(test.String)
		}
		if results.Valid && results.String != "" {
			pe.Results = json.RawMessage(results.String)
		}
		if arms.Valid && arms.String != "" {
			pe.Arms = json.RawMessage(arms.String)
		}
		var err error
		if pe.OpenedAt, err = parseTime(opened); err != nil {
			return err
		}
		if pe.DecideBy, err = parseTime(decideBy); err != nil {
			return err
		}
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
