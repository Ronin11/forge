package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	"forge/internal/core/model"
)

// WorkflowRun is one execution of a workflow graph: the engine's durable
// cursor. The graph is frozen at creation so a later edit never reroutes a
// run in flight; status is written by the engine, never derived.
type WorkflowRun struct {
	ID                 string         `json:"id"`
	WorkflowID         string         `json:"workflow_id"`
	WorkflowName       string         `json:"workflow_name"`
	WorkflowGeneration int            `json:"workflow_generation"`
	Graph              *WorkflowGraph `json:"graph,omitempty"`
	Context            RunContext     `json:"context"`
	Status             string         `json:"status"`
	Trigger            model.Trigger  `json:"trigger"`
	ScriptRuns         int            `json:"script_runs"`
	CreatedAt          time.Time      `json:"created_at"`
	UpdatedAt          time.Time      `json:"updated_at"`
	FinishedAt         time.Time      `json:"finished_at,omitempty"`
}

// RunContext is what the caller provided at run time; routine nodes inherit
// it unless their config overrides.
type RunContext struct {
	Repositories []string `json:"repositories,omitempty"`
	Objective    string   `json:"objective,omitempty"`
}

// Run statuses. A run is running until every reachable node is terminal.
const (
	RunRunning   = "running"
	RunSucceeded = "succeeded"
	RunFailed    = "failed"
	RunCancelled = "cancelled"
)

// RunNode is one node instance within a run. A node executes once per
// instance; a loop re-entry creates the next iteration. Edges records the
// engine's decision for each incoming edge key (taken|dead) — the token
// bookkeeping that makes readiness restart-safe.
type RunNode struct {
	ID         string            `json:"id"`
	RunID      string            `json:"run_id"`
	NodeID     string            `json:"node_id"`
	Iteration  int               `json:"iteration"`
	Type       NodeType          `json:"type"`
	Status     string            `json:"status"`
	WorkID     string            `json:"work_id,omitempty"`
	Edges      map[string]string `json:"edges,omitempty"`
	Output     json.RawMessage   `json:"output,omitempty"`
	Error      string            `json:"error,omitempty"`
	StartedAt  time.Time         `json:"started_at,omitempty"`
	FinishedAt time.Time         `json:"finished_at,omitempty"`
	CreatedAt  time.Time         `json:"created_at"`
	UpdatedAt  time.Time         `json:"updated_at"`
}

// Node instance statuses.
const (
	NodePending   = "pending" // created by a token, waiting for more edges
	NodeReady     = "ready"   // all conditions met; awaiting materialization
	NodeRunning   = "running" // routine: Work in flight
	NodeSucceeded = "succeeded"
	NodeFailed    = "failed"
	NodeSkipped   = "skipped" // every incoming edge went dead
	NodeCancelled = "cancelled"
)

// NodeTerminal reports whether an instance status is final.
func NodeTerminal(status string) bool {
	switch status {
	case NodeSucceeded, NodeFailed, NodeSkipped, NodeCancelled:
		return true
	}
	return false
}

// MaxNodeOutputBytes caps one node's stored output.
const MaxNodeOutputBytes = 64 * 1024

// Per-run guardrails: a runaway loop fails loudly instead of grinding.
const (
	MaxRunInstances = 200
	MaxRunWorks     = 100
	MaxRunScripts   = 100
)

// CreateWorkflowRun freezes the graph and context into a run row.
func (tx *Tx) CreateWorkflowRun(ctx context.Context, r *WorkflowRun) error {
	if r.ID == "" {
		r.ID = model.NewID()
	}
	if r.Status == "" {
		r.Status = RunRunning
	}
	r.CreatedAt, r.UpdatedAt = tx.now, tx.now
	graph, err := json.Marshal(r.Graph)
	if err != nil {
		return fmt.Errorf("encode run graph: %w", err)
	}
	rctx, err := json.Marshal(r.Context)
	if err != nil {
		return fmt.Errorf("encode run context: %w", err)
	}
	_, err = tx.Exec(ctx, `INSERT INTO workflow_runs (id, workflow_id, workflow_name, workflow_generation, graph, context, status, trigger, script_runs, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		r.ID, r.WorkflowID, r.WorkflowName, r.WorkflowGeneration, string(graph), string(rctx), r.Status, string(r.Trigger), r.ScriptRuns, formatTime(r.CreatedAt), formatTime(r.UpdatedAt))
	if err != nil {
		return fmt.Errorf("insert workflow run %s: %w", r.ID, err)
	}
	return nil
}

const workflowRunColumns = `id, workflow_id, workflow_name, workflow_generation, graph, context, status, trigger, script_runs, created_at, updated_at, finished_at`

// GetWorkflowRun reads one run by id (reader pool).
func (s *Store) GetWorkflowRun(ctx context.Context, id string) (*WorkflowRun, error) {
	rs, err := scanWorkflowRuns(each(s.query(ctx, `SELECT `+workflowRunColumns+` FROM workflow_runs WHERE id = ?`, id)))
	return oneWorkflowRun(rs, err, id)
}

// GetWorkflowRun reads one run inside a write transaction.
func (tx *Tx) GetWorkflowRun(ctx context.Context, id string) (*WorkflowRun, error) {
	rs, err := scanWorkflowRuns(each(tx.Query(ctx, `SELECT `+workflowRunColumns+` FROM workflow_runs WHERE id = ?`, id)))
	return oneWorkflowRun(rs, err, id)
}

func oneWorkflowRun(rs []WorkflowRun, err error, id string) (*WorkflowRun, error) {
	if err != nil {
		return nil, err
	}
	if len(rs) == 0 {
		return nil, fmt.Errorf("workflow run %s: %w", id, ErrNotFound)
	}
	return &rs[0], nil
}

// OpenWorkflowRuns returns the ids of every run the engine still owes work.
func (s *Store) OpenWorkflowRuns(ctx context.Context) ([]string, error) {
	var ids []string
	err := each(s.query(ctx, `SELECT id FROM workflow_runs WHERE status = ?`, RunRunning))(func(rows *sql.Rows) error {
		var id string
		if err := rows.Scan(&id); err != nil {
			return err
		}
		ids = append(ids, id)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read open workflow runs: %w", err)
	}
	return ids, nil
}

// HasOpenWorkflowRun reports whether a workflow has a run still going — the
// scheduler's skip-if-running check.
func (s *Store) HasOpenWorkflowRun(ctx context.Context, name string) (bool, error) {
	var n int
	if err := s.queryRow(ctx, `SELECT count(*) FROM workflow_runs WHERE workflow_name = ? AND status = ?`, name, RunRunning).Scan(&n); err != nil {
		return false, fmt.Errorf("count open runs of %s: %w", name, err)
	}
	return n > 0, nil
}

// WorkflowRunsFor lists a workflow's runs, newest first.
func (s *Store) WorkflowRunsFor(ctx context.Context, name string, limit int) ([]WorkflowRun, error) {
	if limit <= 0 || limit > 500 {
		limit = 100
	}
	return scanWorkflowRuns(each(s.query(ctx, `SELECT `+workflowRunColumns+` FROM workflow_runs WHERE workflow_name = ? ORDER BY created_at DESC, id DESC LIMIT ?`, name, limit)))
}

// SetWorkflowRunStatus finalizes or re-opens a run. Terminal statuses stamp
// finished_at once.
func (tx *Tx) SetWorkflowRunStatus(ctx context.Context, id, status string) error {
	finished := any(nil)
	if status != RunRunning {
		finished = formatTime(tx.now)
	}
	res, err := tx.Exec(ctx, `UPDATE workflow_runs SET status = ?, finished_at = ?, updated_at = ? WHERE id = ?`, status, finished, formatTime(tx.now), id)
	if err != nil {
		return fmt.Errorf("set run %s status: %w", id, err)
	}
	return oneRow(res, "workflow run "+id)
}

// AddWorkflowRunScripts bumps the per-run script budget counter.
func (tx *Tx) AddWorkflowRunScripts(ctx context.Context, id string, n int) error {
	res, err := tx.Exec(ctx, `UPDATE workflow_runs SET script_runs = script_runs + ?, updated_at = ? WHERE id = ?`, n, formatTime(tx.now), id)
	if err != nil {
		return fmt.Errorf("count scripts of run %s: %w", id, err)
	}
	return oneRow(res, "workflow run "+id)
}

// CreateRunNode inserts one node instance. The UNIQUE(run_id, node_id,
// iteration) constraint is the engine's duplicate-instance guard: a lost race
// surfaces as ErrConflict and the loser re-reads.
func (tx *Tx) CreateRunNode(ctx context.Context, n *RunNode) error {
	if n.ID == "" {
		n.ID = model.NewID()
	}
	if n.Iteration == 0 {
		n.Iteration = 1
	}
	if n.Edges == nil {
		n.Edges = map[string]string{}
	}
	n.CreatedAt, n.UpdatedAt = tx.now, tx.now
	edges, err := json.Marshal(n.Edges)
	if err != nil {
		return fmt.Errorf("encode node edges: %w", err)
	}
	_, err = tx.Exec(ctx, `INSERT INTO workflow_run_nodes (id, run_id, node_id, iteration, type, status, work_id, edges, output, error, started_at, finished_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		n.ID, n.RunID, n.NodeID, n.Iteration, string(n.Type), n.Status, nullString(n.WorkID), string(edges), nullString(string(n.Output)), nullString(n.Error), nullTime(n.StartedAt), nullTime(n.FinishedAt), formatTime(n.CreatedAt), formatTime(n.UpdatedAt))
	if err != nil {
		if isUniqueViolation(err) {
			return fmt.Errorf("run %s node %s iteration %d already exists: %w", n.RunID, n.NodeID, n.Iteration, ErrConflict)
		}
		return fmt.Errorf("insert run node %s/%s: %w", n.RunID, n.NodeID, err)
	}
	return nil
}

// UpdateRunNodeFrom writes an instance's mutable fields with a compare-and-set
// on the status it is leaving — the engine's idempotence anchor. A lost race
// returns ErrStaleGeneration; the caller re-reads and re-evaluates.
func (tx *Tx) UpdateRunNodeFrom(ctx context.Context, n *RunNode, fromStatus string) error {
	if len(n.Output) > MaxNodeOutputBytes {
		return fmt.Errorf("node %s output %d bytes exceeds %d", n.NodeID, len(n.Output), MaxNodeOutputBytes)
	}
	n.UpdatedAt = tx.now
	edges, err := json.Marshal(n.Edges)
	if err != nil {
		return fmt.Errorf("encode node edges: %w", err)
	}
	res, err := tx.Exec(ctx, `UPDATE workflow_run_nodes SET status = ?, work_id = ?, edges = ?, output = ?, error = ?, started_at = ?, finished_at = ?, updated_at = ? WHERE id = ? AND status = ?`,
		n.Status, nullString(n.WorkID), string(edges), nullString(string(n.Output)), nullString(n.Error), nullTime(n.StartedAt), nullTime(n.FinishedAt), formatTime(n.UpdatedAt), n.ID, fromStatus)
	if err != nil {
		return fmt.Errorf("update run node %s: %w", n.ID, err)
	}
	rows, err := res.RowsAffected()
	if err != nil {
		return fmt.Errorf("update run node %s: %w", n.ID, err)
	}
	if rows != 1 {
		return fmt.Errorf("run node %s left %s concurrently: %w", n.ID, fromStatus, ErrStaleGeneration)
	}
	return nil
}

// RunNodes reads every instance of a run (reader pool), oldest first.
func (s *Store) RunNodes(ctx context.Context, runID string) ([]RunNode, error) {
	return scanRunNodes(each(s.query(ctx, `SELECT `+runNodeColumns+` FROM workflow_run_nodes WHERE run_id = ? ORDER BY created_at, iteration`, runID)))
}

// RunNodes reads inside a write transaction (the engine's write pass re-read).
func (tx *Tx) RunNodes(ctx context.Context, runID string) ([]RunNode, error) {
	return scanRunNodes(each(tx.Query(ctx, `SELECT `+runNodeColumns+` FROM workflow_run_nodes WHERE run_id = ? ORDER BY created_at, iteration`, runID)))
}

// WorkForRun returns the Works a run materialized, any state.
func (s *Store) WorkForRun(ctx context.Context, runID string) ([]Work, error) {
	return scanWork(each(s.query(ctx, `SELECT `+workColumns+` FROM work WHERE workflow_run_id = ? ORDER BY rowid`, runID)))
}

const runNodeColumns = `id, run_id, node_id, iteration, type, status, work_id, edges, output, error, started_at, finished_at, created_at, updated_at`

func scanRunNodes(iter func(func(*sql.Rows) error) error) ([]RunNode, error) {
	var out []RunNode
	err := iter(func(rows *sql.Rows) error {
		var n RunNode
		var typ, status string
		var workID, edges, output, errMsg, started, finished, created, updated sql.NullString
		if err := rows.Scan(&n.ID, &n.RunID, &n.NodeID, &n.Iteration, &typ, &status, &workID, &edges, &output, &errMsg, &started, &finished, &created, &updated); err != nil {
			return fmt.Errorf("scan run node: %w", err)
		}
		n.Type, n.Status, n.WorkID, n.Error = NodeType(typ), status, workID.String, errMsg.String
		if edges.Valid && edges.String != "" {
			if err := json.Unmarshal([]byte(edges.String), &n.Edges); err != nil {
				return fmt.Errorf("decode edges of node %s: %w", n.ID, err)
			}
		}
		if output.Valid && output.String != "" {
			n.Output = json.RawMessage(output.String)
		}
		var err error
		for _, p := range []struct {
			dst *time.Time
			src sql.NullString
		}{{&n.StartedAt, started}, {&n.FinishedAt, finished}, {&n.CreatedAt, created}, {&n.UpdatedAt, updated}} {
			if *p.dst, err = parseTime(p.src); err != nil {
				return err
			}
		}
		out = append(out, n)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read run nodes: %w", err)
	}
	return out, nil
}

func scanWorkflowRuns(iter func(func(*sql.Rows) error) error) ([]WorkflowRun, error) {
	var out []WorkflowRun
	err := iter(func(rows *sql.Rows) error {
		var r WorkflowRun
		var graph, rctx, trigger string
		var finished, created, updated sql.NullString
		if err := rows.Scan(&r.ID, &r.WorkflowID, &r.WorkflowName, &r.WorkflowGeneration, &graph, &rctx, &r.Status, &trigger, &r.ScriptRuns, &created, &updated, &finished); err != nil {
			return fmt.Errorf("scan workflow run: %w", err)
		}
		if err := json.Unmarshal([]byte(graph), &r.Graph); err != nil {
			return fmt.Errorf("decode graph of run %s: %w", r.ID, err)
		}
		if err := json.Unmarshal([]byte(rctx), &r.Context); err != nil {
			return fmt.Errorf("decode context of run %s: %w", r.ID, err)
		}
		r.Trigger = model.Trigger(trigger)
		var err error
		for _, p := range []struct {
			dst *time.Time
			src sql.NullString
		}{{&r.FinishedAt, finished}, {&r.CreatedAt, created}, {&r.UpdatedAt, updated}} {
			if *p.dst, err = parseTime(p.src); err != nil {
				return err
			}
		}
		out = append(out, r)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read workflow runs: %w", err)
	}
	return out, nil
}
