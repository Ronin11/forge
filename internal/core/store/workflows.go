package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"forge/internal/core/model"
)

// Workflow is routines strung together: an ordered list of steps, each naming a
// routine and the steps it runs after. Running one instantiates one Work per
// step with blocked_by edges (handlers_workflows.go); a run has no state of its
// own — it is the derived states of its Works, grouped by workflow_run_id.
type Workflow struct {
	ID              string         `json:"id"`
	Name            string         `json:"name"`
	Steps           []WorkflowStep `json:"steps"`
	Schedule        string         `json:"schedule,omitempty"`
	ScheduleEnabled bool           `json:"schedule_enabled"`
	Generation      int            `json:"generation"`
	NextDueAt       time.Time      `json:"next_due_at,omitempty"`
	ArchivedAt      time.Time      `json:"archived_at,omitempty"`
	CreatedAt       time.Time      `json:"created_at"`
	UpdatedAt       time.Time      `json:"updated_at"`
}

// WorkflowStep is one routine invocation within a workflow. After nil on any
// step but the first means "after the previous step" — a plain list is a chain;
// an explicit empty list makes the step a root that starts immediately.
type WorkflowStep struct {
	Name    string         `json:"name" toml:"name"`
	Routine string         `json:"routine" toml:"routine"`
	After   []WorkflowEdge `json:"after,omitempty" toml:"after"`
}

// WorkflowEdge names an earlier step this one is blocked by, with the same
// semantics as a Work dependency (model.Edge).
type WorkflowEdge struct {
	Step    string             `json:"step" toml:"step"`
	On      model.DependencyOn `json:"on,omitempty" toml:"on"`
	StackOn bool               `json:"stack_on,omitempty" toml:"stack_on"`
}

// MaxWorkflowSteps bounds a workflow: one run creates this many Works at most.
const MaxWorkflowSteps = 50

// normalize materializes the defaults before validation so the stored form is
// explicit: a step with no `after` follows the previous step (a plain list is a
// chain; `after = []` makes an independent root), and an edge's empty `on` is
// success.
func (w *Workflow) normalize() {
	for i := range w.Steps {
		if i > 0 && w.Steps[i].After == nil {
			w.Steps[i].After = []WorkflowEdge{{Step: w.Steps[i-1].Name, On: model.OnSuccess}}
		}
		for j := range w.Steps[i].After {
			if w.Steps[i].After[j].On == "" {
				w.Steps[i].After[j].On = model.OnSuccess
			}
		}
	}
}

// Validate is what the API and CLI reject on before anything is stored. Edges
// may only reference earlier steps, so a stored workflow is a DAG by
// construction and instantiation can walk the steps in order.
func (w *Workflow) Validate() error {
	if err := model.ValidateName(w.Name); err != nil {
		return err
	}
	if len(w.Steps) == 0 {
		return fmt.Errorf("workflow %s: at least one step is required", w.Name)
	}
	if len(w.Steps) > MaxWorkflowSteps {
		return fmt.Errorf("workflow %s: %d steps exceed %d", w.Name, len(w.Steps), MaxWorkflowSteps)
	}
	seen := map[string]bool{}
	for i, st := range w.Steps {
		if err := model.ValidateName(st.Name); err != nil {
			return fmt.Errorf("workflow %s: step %d: %w", w.Name, i+1, err)
		}
		if seen[st.Name] {
			return fmt.Errorf("workflow %s: step %s listed twice", w.Name, st.Name)
		}
		if err := model.ValidateName(st.Routine); err != nil {
			return fmt.Errorf("workflow %s: step %s: routine: %w", w.Name, st.Name, err)
		}
		for _, e := range st.After {
			if !seen[e.Step] {
				return fmt.Errorf("workflow %s: step %s: after %q must name an earlier step", w.Name, st.Name, e.Step)
			}
			switch e.On {
			case "", model.OnSuccess, model.OnTerminal:
			default:
				return fmt.Errorf("workflow %s: step %s: on %q: want success or terminal", w.Name, st.Name, e.On)
			}
		}
		seen[st.Name] = true
	}
	return nil
}

// CreateWorkflow inserts a workflow at generation 1 and records the generation.
func (tx *Tx) CreateWorkflow(ctx context.Context, w *Workflow) error {
	w.normalize()
	if err := w.Validate(); err != nil {
		return err
	}
	if w.ID == "" {
		w.ID = model.NewID()
	}
	w.Generation = 1
	w.CreatedAt, w.UpdatedAt = tx.now, tx.now
	steps, err := json.Marshal(w.Steps)
	if err != nil {
		return fmt.Errorf("encode steps of %s: %w", w.Name, err)
	}
	_, err = tx.Exec(ctx, `INSERT INTO workflows (id, name, steps, schedule, schedule_enabled, generation, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
		w.ID, w.Name, string(steps), nullString(w.Schedule), boolInt(w.ScheduleEnabled), w.Generation, formatTime(w.CreatedAt), formatTime(w.UpdatedAt))
	if err != nil {
		if isUniqueViolation(err) {
			return fmt.Errorf("workflow %s already exists: %w", w.Name, ErrConflict)
		}
		return fmt.Errorf("insert workflow %s: %w", w.Name, err)
	}
	return tx.recordWorkflowGeneration(ctx, w, "edit")
}

// UpdateWorkflow replaces every editable field, requires the caller's expected
// generation (409 otherwise), and bumps the generation.
// UpdateWorkflowFrom is UpdateWorkflow with an explicit generation source
// (`proposal:<id>` when an approved proposal applies).
func (tx *Tx) UpdateWorkflowFrom(ctx context.Context, w *Workflow, expectedGeneration int, source string) error {
	return tx.updateWorkflow(ctx, w, expectedGeneration, source)
}

func (tx *Tx) UpdateWorkflow(ctx context.Context, w *Workflow, expectedGeneration int) error {
	return tx.updateWorkflow(ctx, w, expectedGeneration, "edit")
}

func (tx *Tx) updateWorkflow(ctx context.Context, w *Workflow, expectedGeneration int, source string) error {
	w.normalize()
	if err := w.Validate(); err != nil {
		return err
	}
	var current int
	err := tx.QueryRow(ctx, `SELECT generation FROM workflows WHERE name = ?`, w.Name).Scan(&current)
	if errors.Is(err, sql.ErrNoRows) {
		return fmt.Errorf("workflow %s: %w", w.Name, ErrNotFound)
	}
	if err != nil {
		return fmt.Errorf("read workflow %s: %w", w.Name, err)
	}
	if current != expectedGeneration {
		return fmt.Errorf("workflow %s is at generation %d, not %d: %w", w.Name, current, expectedGeneration, ErrStaleGeneration)
	}
	w.Generation = current + 1
	w.UpdatedAt = tx.now
	steps, err := json.Marshal(w.Steps)
	if err != nil {
		return fmt.Errorf("encode steps of %s: %w", w.Name, err)
	}
	_, err = tx.Exec(ctx, `UPDATE workflows SET steps=?, schedule=?, schedule_enabled=?, generation=?, updated_at=? WHERE name=?`,
		string(steps), nullString(w.Schedule), boolInt(w.ScheduleEnabled), w.Generation, formatTime(w.UpdatedAt), w.Name)
	if err != nil {
		return fmt.Errorf("update workflow %s: %w", w.Name, err)
	}
	return tx.recordWorkflowGeneration(ctx, w, source)
}

func (tx *Tx) recordWorkflowGeneration(ctx context.Context, w *Workflow, source string) error {
	snap, err := json.Marshal(w)
	if err != nil {
		return fmt.Errorf("snapshot workflow %s: %w", w.Name, err)
	}
	if _, err := tx.Exec(ctx, `INSERT INTO workflow_generations (workflow_id, generation, snapshot, source, created_at) VALUES (?, ?, ?, ?, ?)`,
		w.ID, w.Generation, string(snap), source, formatTime(tx.now)); err != nil {
		return fmt.Errorf("record generation %d of %s: %w", w.Generation, w.Name, err)
	}
	return nil
}

// ArchiveWorkflow disables the schedule and blocks new runs; history stays.
func (tx *Tx) ArchiveWorkflow(ctx context.Context, name string) error {
	res, err := tx.Exec(ctx, `UPDATE workflows SET archived_at = ?, schedule_enabled = 0, updated_at = ? WHERE name = ? AND archived_at IS NULL`, formatTime(tx.now), formatTime(tx.now), name)
	if err != nil {
		return fmt.Errorf("archive workflow %s: %w", name, err)
	}
	return oneRow(res, "workflow "+name)
}

// SetWorkflowScheduleEnabled flips the schedule without a generation bump: it
// changes when the workflow runs, not what it does.
func (tx *Tx) SetWorkflowScheduleEnabled(ctx context.Context, name string, enabled bool) error {
	res, err := tx.Exec(ctx, `UPDATE workflows SET schedule_enabled = ?, updated_at = ? WHERE name = ? AND archived_at IS NULL`, boolInt(enabled), formatTime(tx.now), name)
	if err != nil {
		return fmt.Errorf("set schedule for %s: %w", name, err)
	}
	return oneRow(res, "workflow "+name)
}

// SetWorkflowNextDue records the next cron occurrence.
func (tx *Tx) SetWorkflowNextDue(ctx context.Context, name string, next time.Time) error {
	if _, err := tx.Exec(ctx, `UPDATE workflows SET next_due_at = ? WHERE name = ?`, nullTime(next), name); err != nil {
		return fmt.Errorf("set next due for %s: %w", name, err)
	}
	return nil
}

const workflowColumns = `id, name, steps, schedule, schedule_enabled, generation, next_due_at, archived_at, created_at, updated_at`

// GetWorkflow reads one workflow by name.
func (s *Store) GetWorkflow(ctx context.Context, name string) (*Workflow, error) {
	ws, err := scanWorkflows(each(s.query(ctx, `SELECT `+workflowColumns+` FROM workflows WHERE name = ?`, name)))
	if err != nil {
		return nil, err
	}
	if len(ws) == 0 {
		return nil, fmt.Errorf("workflow %s: %w", name, ErrNotFound)
	}
	return &ws[0], nil
}

// GetWorkflow reads inside a write transaction (run-now instantiation).
func (tx *Tx) GetWorkflow(ctx context.Context, name string) (*Workflow, error) {
	ws, err := scanWorkflows(each(tx.Query(ctx, `SELECT `+workflowColumns+` FROM workflows WHERE name = ?`, name)))
	if err != nil {
		return nil, err
	}
	if len(ws) == 0 {
		return nil, fmt.Errorf("workflow %s: %w", name, ErrNotFound)
	}
	return &ws[0], nil
}

// ListWorkflows returns workflows by name; archived ones only when asked.
func (s *Store) ListWorkflows(ctx context.Context, includeArchived bool) ([]Workflow, error) {
	q := `SELECT ` + workflowColumns + ` FROM workflows`
	if !includeArchived {
		q += ` WHERE archived_at IS NULL`
	}
	return scanWorkflows(each(s.query(ctx, q+` ORDER BY name`)))
}

// DueWorkflows returns enabled, unarchived workflows whose next_due_at ≤ now.
func (s *Store) DueWorkflows(ctx context.Context, now time.Time) ([]Workflow, error) {
	return scanWorkflows(each(s.query(ctx, `SELECT `+workflowColumns+` FROM workflows WHERE schedule_enabled = 1 AND archived_at IS NULL AND next_due_at IS NOT NULL AND next_due_at <= ? ORDER BY next_due_at LIMIT 100`, formatTime(now))))
}

// WorkForWorkflow returns the most recent Work stamped with the workflow's
// name, newest first — the runs listing's input, grouped by workflow_run_id.
// Ordering is by rowid, not created_at: a run's Works are created in one
// transaction and share a timestamp, while insertion order is step order.
func (s *Store) WorkForWorkflow(ctx context.Context, name string, limit int) ([]Work, error) {
	if limit <= 0 || limit > 500 {
		limit = 100
	}
	return scanWork(each(s.query(ctx, `SELECT `+workColumns+` FROM work WHERE workflow_name = ? ORDER BY rowid DESC LIMIT ?`, name, limit)))
}

func scanWorkflows(iter func(func(*sql.Rows) error) error) ([]Workflow, error) {
	var out []Workflow
	err := iter(func(rows *sql.Rows) error {
		var w Workflow
		var steps string
		var schedule, nextDue, archived, created, updated sql.NullString
		var enabled int
		if err := rows.Scan(&w.ID, &w.Name, &steps, &schedule, &enabled, &w.Generation, &nextDue, &archived, &created, &updated); err != nil {
			return fmt.Errorf("scan workflow: %w", err)
		}
		if err := json.Unmarshal([]byte(steps), &w.Steps); err != nil {
			return fmt.Errorf("decode steps of %s: %w", w.Name, err)
		}
		w.Schedule, w.ScheduleEnabled = schedule.String, enabled == 1
		var err error
		for _, p := range []struct {
			dst *time.Time
			src sql.NullString
		}{{&w.NextDueAt, nextDue}, {&w.ArchivedAt, archived}, {&w.CreatedAt, created}, {&w.UpdatedAt, updated}} {
			if *p.dst, err = parseTime(p.src); err != nil {
				return err
			}
		}
		out = append(out, w)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read workflows: %w", err)
	}
	return out, nil
}
