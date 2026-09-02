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

// Workflow is a named graph of nodes (workflow_graph.go): routine nodes run
// Works through the queue; script, switch, and join nodes are evaluated by the
// run engine. Graph is the canonical form; Steps is accepted on input (and in
// old generation snapshots) and converted by normalize, never emitted.
type Workflow struct {
	ID              string         `json:"id"`
	Name            string         `json:"name"`
	Graph           *WorkflowGraph `json:"graph,omitempty"`
	Steps           []WorkflowStep `json:"steps,omitempty"`
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

// MaxWorkflowSteps bounds the legacy steps form; the graph form is bounded by
// MaxGraphNodes.
const MaxWorkflowSteps = 50

// Normalize converts to the canonical form: a legacy steps list (a step with
// no `after` follows the previous step; a plain list is a chain; `after = []`
// makes an independent root; an edge's empty `on` is success) becomes a graph
// via GraphFromSteps, and Steps is cleared — the graph is the one stored form.
// Create/Update run it themselves; the API also runs it at decode time so
// handlers always see the graph.
func (w *Workflow) Normalize() error {
	if w.Graph != nil {
		w.Steps = nil
		return nil
	}
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
	if err := validateSteps(w.Name, w.Steps); err != nil {
		return err
	}
	w.Graph = GraphFromSteps(w.Steps)
	w.Steps = nil
	return nil
}

// validateSteps checks the legacy steps form before conversion: edges may only
// reference earlier steps, so the converted graph is a DAG by construction.
func validateSteps(name string, steps []WorkflowStep) error {
	if len(steps) > MaxWorkflowSteps {
		return fmt.Errorf("workflow %s: %d steps exceed %d", name, len(steps), MaxWorkflowSteps)
	}
	seen := map[string]bool{}
	for i, st := range steps {
		if err := model.ValidateName(st.Name); err != nil {
			return fmt.Errorf("workflow %s: step %d: %w", name, i+1, err)
		}
		if seen[st.Name] {
			return fmt.Errorf("workflow %s: step %s listed twice", name, st.Name)
		}
		if err := model.ValidateName(st.Routine); err != nil {
			return fmt.Errorf("workflow %s: step %s: routine: %w", name, st.Name, err)
		}
		for _, e := range st.After {
			if !seen[e.Step] {
				return fmt.Errorf("workflow %s: step %s: after %q must name an earlier step", name, st.Name, e.Step)
			}
			switch e.On {
			case "", model.OnSuccess, model.OnTerminal:
			default:
				return fmt.Errorf("workflow %s: step %s: on %q: want success or terminal", name, st.Name, e.On)
			}
		}
		seen[st.Name] = true
	}
	return nil
}

// Validate is what the API and CLI reject on before anything is stored.
// Callers run normalize first, so the graph is present and canonical.
func (w *Workflow) Validate() error {
	if err := model.ValidateName(w.Name); err != nil {
		return err
	}
	if w.Graph == nil || len(w.Graph.Nodes) == 0 {
		return fmt.Errorf("workflow %s: at least one node is required", w.Name)
	}
	if err := w.Graph.Validate(); err != nil {
		return fmt.Errorf("workflow %s: %w", w.Name, err)
	}
	if err := ValidateSchedule(w.Schedule); err != nil {
		return fmt.Errorf("workflow %s: %w", w.Name, err)
	}
	return nil
}

// CreateWorkflow inserts a workflow at generation 1 and records the generation.
func (tx *Tx) CreateWorkflow(ctx context.Context, w *Workflow) error {
	if err := w.Normalize(); err != nil {
		return err
	}
	if err := w.Validate(); err != nil {
		return err
	}
	if w.ID == "" {
		w.ID = model.NewID()
	}
	w.Generation = 1
	w.CreatedAt, w.UpdatedAt = tx.now, tx.now
	graph, err := json.Marshal(w.Graph)
	if err != nil {
		return fmt.Errorf("encode graph of %s: %w", w.Name, err)
	}
	// `steps` is written empty: the graph is canonical, the column stays NOT
	// NULL for old readers and generation snapshots.
	_, err = tx.Exec(ctx, `INSERT INTO workflows (id, name, steps, graph, schedule, schedule_enabled, generation, created_at, updated_at) VALUES (?, ?, '[]', ?, ?, ?, ?, ?, ?)`,
		w.ID, w.Name, string(graph), nullString(w.Schedule), boolInt(w.ScheduleEnabled), w.Generation, formatTime(w.CreatedAt), formatTime(w.UpdatedAt))
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
	if err := w.Normalize(); err != nil {
		return err
	}
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
	graph, err := json.Marshal(w.Graph)
	if err != nil {
		return fmt.Errorf("encode graph of %s: %w", w.Name, err)
	}
	_, err = tx.Exec(ctx, `UPDATE workflows SET steps='[]', graph=?, schedule=?, schedule_enabled=?, generation=?, updated_at=? WHERE name=?`,
		string(graph), nullString(w.Schedule), boolInt(w.ScheduleEnabled), w.Generation, formatTime(w.UpdatedAt), w.Name)
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

// SetWorkflowPositions moves nodes on the editor canvas without a generation
// bump (the SetWorkflowScheduleEnabled precedent: it changes how the graph
// looks, not what it does). Unknown node ids are refused so a stale editor
// cannot silently no-op.
func (tx *Tx) SetWorkflowPositions(ctx context.Context, name string, positions map[string]GraphPosition) error {
	w, err := tx.GetWorkflow(ctx, name)
	if err != nil {
		return err
	}
	if !w.ArchivedAt.IsZero() {
		return fmt.Errorf("workflow %s is archived: %w", name, ErrConflict)
	}
	for id, p := range positions {
		n := w.Graph.Node(id)
		if n == nil {
			return fmt.Errorf("workflow %s has no node %q: %w", name, id, ErrNotFound)
		}
		n.Position = p
	}
	graph, err := json.Marshal(w.Graph)
	if err != nil {
		return fmt.Errorf("encode graph of %s: %w", name, err)
	}
	if _, err := tx.Exec(ctx, `UPDATE workflows SET graph = ?, updated_at = ? WHERE name = ?`, string(graph), formatTime(tx.now), name); err != nil {
		return fmt.Errorf("update layout of %s: %w", name, err)
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

const workflowColumns = `id, name, steps, graph, schedule, schedule_enabled, generation, next_due_at, archived_at, created_at, updated_at`

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

// backfillWorkflowGraphs persists the steps→graph conversion for rows written
// before the graph column existed. Open calls it after migrations; it is
// idempotent (graph IS NULL guards it) and journals each conversion.
func (s *Store) backfillWorkflowGraphs(ctx context.Context) error {
	names := []string{}
	err := each(s.query(ctx, `SELECT name FROM workflows WHERE graph IS NULL`))(func(rows *sql.Rows) error {
		var n string
		if err := rows.Scan(&n); err != nil {
			return err
		}
		names = append(names, n)
		return nil
	})
	if err != nil {
		return fmt.Errorf("find graphless workflows: %w", err)
	}
	if len(names) == 0 {
		return nil
	}
	return s.Write(ctx, func(tx *Tx) error {
		for _, name := range names {
			w, err := tx.GetWorkflow(ctx, name) // scan converts steps → graph
			if err != nil {
				return err
			}
			graph, err := json.Marshal(w.Graph)
			if err != nil {
				return fmt.Errorf("encode graph of %s: %w", name, err)
			}
			if _, err := tx.Exec(ctx, `UPDATE workflows SET graph = ? WHERE name = ? AND graph IS NULL`, string(graph), name); err != nil {
				return fmt.Errorf("backfill graph of %s: %w", name, err)
			}
			if err := tx.Journal(ctx, "workflow.graph_migrated", EntityWorkflow, w.ID, map[string]any{"workflow": name, "nodes": len(w.Graph.Nodes)}); err != nil {
				return err
			}
		}
		return nil
	})
}

func scanWorkflows(iter func(func(*sql.Rows) error) error) ([]Workflow, error) {
	var out []Workflow
	err := iter(func(rows *sql.Rows) error {
		var w Workflow
		var steps string
		var graph, schedule, nextDue, archived, created, updated sql.NullString
		var enabled int
		if err := rows.Scan(&w.ID, &w.Name, &steps, &graph, &schedule, &enabled, &w.Generation, &nextDue, &archived, &created, &updated); err != nil {
			return fmt.Errorf("scan workflow: %w", err)
		}
		if graph.Valid && graph.String != "" {
			if err := json.Unmarshal([]byte(graph.String), &w.Graph); err != nil {
				return fmt.Errorf("decode graph of %s: %w", w.Name, err)
			}
		} else {
			// A pre-backfill row: convert on read so no caller ever sees a
			// graphless workflow (backfillWorkflowGraphs persists this).
			if err := json.Unmarshal([]byte(steps), &w.Steps); err != nil {
				return fmt.Errorf("decode steps of %s: %w", w.Name, err)
			}
			if err := w.Normalize(); err != nil {
				return fmt.Errorf("convert steps of %s: %w", w.Name, err)
			}
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
