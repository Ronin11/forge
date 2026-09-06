package web

import (
	"errors"
	"fmt"
	"net/http"
	"sort"
	"strconv"
	"strings"
	"time"

	"forge/internal/core/engine"
	"forge/internal/core/model"
	"forge/internal/core/store"
)

// Workflows are graphs of typed nodes (store.WorkflowGraph). Running one
// creates a first-class run row with the graph frozen; the run engine
// (flow_engine.go) materializes routine nodes into Works as they become
// ready, so the queue, dependency, and attention machinery treat them like
// any other Work while switches, scripts, failure edges, and loops are
// evaluated between materializations.

func (s *Server) listWorkflows(r *http.Request) (int, any, error) {
	workflows, err := s.store.ListWorkflows(r.Context(), r.URL.Query().Get("archived") == "true")
	if err != nil {
		return 0, nil, err
	}
	if workflows == nil {
		workflows = []store.Workflow{}
	}
	return http.StatusOK, workflows, nil
}

// decodeWorkflow reads a workflow body — graph form or legacy steps form — and
// normalizes to the canonical graph so every handler sees one shape.
func decodeWorkflow(r *http.Request) (*store.Workflow, error) {
	var wf store.Workflow
	if err := decodeJSON(r, &wf); err != nil {
		return nil, err
	}
	wf.ID, wf.Generation = "", 0
	if err := model.ValidateName(wf.Name); err != nil {
		return nil, badRequest("%v", err)
	}
	return &wf, nil
}

func (s *Server) createWorkflow(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	wf, err := decodeWorkflow(r)
	if err != nil {
		return 0, nil, err
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		if err := s.checkGraphNodes(wf); err != nil {
			return err
		}
		return routineWriteError(tx.CreateWorkflow(ctx, wf))
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "workflow created", "workflow", wf.Name, "workflow_id", wf.ID, "nodes", len(wf.Graph.Nodes))
	return http.StatusCreated, wf, nil
}

func (s *Server) getWorkflow(r *http.Request) (int, any, error) {
	wf, err := s.store.GetWorkflow(r.Context(), r.PathValue("name"))
	if err != nil {
		return 0, nil, err
	}
	return http.StatusOK, wf, nil
}

func (s *Server) updateWorkflow(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	generation, err := strconv.Atoi(r.URL.Query().Get("generation"))
	if err != nil || generation <= 0 {
		return 0, nil, badRequest("generation query parameter is required: the generation you edited")
	}
	wf, err := decodeWorkflow(r)
	if err != nil {
		return 0, nil, err
	}
	if wf.Name != r.PathValue("name") {
		return 0, nil, badRequest("workflow name %q does not match the path", wf.Name)
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		saved, err := tx.GetWorkflow(ctx, wf.Name)
		if err != nil {
			return err
		}
		wf.ID = saved.ID // the generation record is keyed by it
		if err := s.checkGraphNodes(wf); err != nil {
			return err
		}
		return routineWriteError(tx.UpdateWorkflow(ctx, wf, generation))
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "workflow updated", "workflow", wf.Name, "generation", wf.Generation)
	return http.StatusOK, wf, nil
}

// updateWorkflowLayout moves nodes on the canvas: {positions: {node: {x,y}}}.
// No generation bump and no generation precondition — dragging nodes around is
// "how it looks", not "what it does", and must not 409 against a real edit.
func (s *Server) updateWorkflowLayout(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	var body struct {
		Positions map[string]store.GraphPosition `json:"positions"`
	}
	if err := decodeJSON(r, &body); err != nil {
		return 0, nil, err
	}
	if len(body.Positions) == 0 {
		return 0, nil, badRequest("positions are required")
	}
	name := r.PathValue("name")
	if err := s.store.Write(ctx, func(tx *store.Tx) error {
		return tx.SetWorkflowPositions(ctx, name, body.Positions)
	}); err != nil {
		return 0, nil, err
	}
	return http.StatusNoContent, nil, nil
}

func (s *Server) archiveWorkflow(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	name := r.PathValue("name")
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.ArchiveWorkflow(ctx, name) }); err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "workflow archived", "workflow", name)
	return http.StatusNoContent, nil, nil
}

// workflowRunCreated is POST /api/v1/workflows/{name}/run's 201 body. The
// run's Works do not exist yet — the engine materializes nodes as they become
// ready; GET /api/v1/workflow-runs/{run_id} follows the run.
type workflowRunCreated struct {
	RunID      string `json:"run_id"`
	Workflow   string `json:"workflow"`
	Generation int    `json:"generation"`
}

// runWorkflow creates the run row — graph frozen, context stored — and hands
// it to the engine. The synchronous advance after commit is a latency
// courtesy: root nodes are materialized before the response so the queue
// shows the run started.
func (s *Server) runWorkflow(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	var body runRequest
	if r.ContentLength != 0 {
		if err := decodeJSON(r, &body); err != nil {
			return 0, nil, err
		}
	}
	name := r.PathValue("name")
	run := &store.WorkflowRun{Trigger: model.TriggerManual, Context: store.RunContext{Repositories: body.Repositories, Objective: body.Objective}}
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		wf, err := tx.GetWorkflow(ctx, name)
		if err != nil {
			return err
		}
		if !wf.ArchivedAt.IsZero() {
			return badRequest("workflow %s is archived", wf.Name)
		}
		// A directive node cannot materialize without a repository — from
		// the run context or its own config; refuse up front instead of
		// minting a run that fails at its first node with no feedback
		// (the silent Run-button bug, 2026-09-06).
		if len(body.Repositories) == 0 {
			for _, n := range wf.Graph.Nodes {
				if n.Type != "directive" {
					continue
				}
				if rs, ok := n.Config["repositories"].([]any); ok && len(rs) > 0 {
					continue
				}
				if rs, ok := n.Config["repositories"].([]string); ok && len(rs) > 0 {
					continue
				}
				return badRequest("workflow %s: node %s has no repository — pass at least one repository for the run", wf.Name, n.ID)
			}
		}
		run.WorkflowID, run.WorkflowName, run.WorkflowGeneration, run.Graph = wf.ID, wf.Name, wf.Generation, wf.Graph
		if err := tx.CreateWorkflowRun(ctx, run); err != nil {
			return err
		}
		return tx.Journal(ctx, "workflow.run_created", store.EntityWorkflow, run.ID, map[string]any{"workflow": wf.Name, "generation": wf.Generation, "trigger": run.Trigger, "nodes": len(wf.Graph.Nodes)})
	})
	if err != nil {
		return 0, nil, err
	}
	s.advanceRun(ctx, run.ID)
	s.log.InfoContext(ctx, "workflow run created", "workflow", name, "run_id", run.ID)
	return http.StatusCreated, workflowRunCreated{RunID: run.ID, Workflow: name, Generation: run.WorkflowGeneration}, nil
}

// runNodeSummary is one node instance in a runs listing or run detail.
type runNodeSummary struct {
	NodeID    string    `json:"node_id"`
	Iteration int       `json:"iteration"`
	Type      string    `json:"type"`
	Status    string    `json:"status"`
	WorkID    string    `json:"work_id,omitempty"`
	Error     string    `json:"error,omitempty"`
	StartedAt time.Time `json:"started_at,omitempty"`
}

// workflowRunDetail is GET /api/v1/workflow-runs/{id}: the run row (frozen
// graph included — the view must draw what routed, not what the workflow says
// today) plus every instance, with the Work summary for routine nodes.
type workflowRunDetail struct {
	store.WorkflowRun
	Nodes []runNodeDetail `json:"nodes"`
}

type runNodeDetail struct {
	store.RunNode
	Work *workSummary `json:"work,omitempty"`
}

func (s *Server) getWorkflowRun(r *http.Request) (int, any, error) {
	ctx := r.Context()
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	run, err := s.store.GetWorkflowRun(ctx, id)
	if err != nil {
		return 0, nil, err
	}
	nodes, err := s.store.RunNodes(ctx, id)
	if err != nil {
		return 0, nil, err
	}
	works, err := s.store.WorkForRun(ctx, id)
	if err != nil {
		return 0, nil, err
	}
	ids := make([]string, len(works))
	byID := map[string]store.Work{}
	for i, w := range works {
		ids[i] = w.ID
		byID[w.ID] = w
	}
	targets, err := s.store.TargetsForWorks(ctx, ids)
	if err != nil {
		return 0, nil, err
	}
	out := workflowRunDetail{WorkflowRun: *run, Nodes: make([]runNodeDetail, 0, len(nodes))}
	for _, n := range nodes {
		d := runNodeDetail{RunNode: n}
		if w, ok := byID[n.WorkID]; ok {
			ts := targets[w.ID]
			if ts == nil {
				ts = []store.Target{}
			}
			state := model.DeriveWorkState(model.WorkInputs{Targets: engine.TargetStates(ts), Integrate: w.Integrate})
			d.Work = &workSummary{Work: w, State: state, Targets: ts}
		}
		out.Nodes = append(out.Nodes, d)
	}
	return http.StatusOK, out, nil
}

// retryWorkflowRun re-opens a terminal run from one failed or cancelled
// node: a fresh instance (iteration + 1, ready — human-initiated, so loop
// caps do not apply to its creation) that the engine materializes and routes
// like any other; downstream nodes re-fire as its tokens reach them.
func (s *Server) retryWorkflowRun(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	var body struct {
		Node string `json:"node"`
	}
	if err := decodeJSON(r, &body); err != nil {
		return 0, nil, err
	}
	if err := model.ValidateName(body.Node); err != nil {
		return 0, nil, badRequest("node: %v", err)
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		run, err := tx.GetWorkflowRun(ctx, id)
		if err != nil {
			return err
		}
		if strings.HasPrefix(run.WorkflowName, "script:") {
			return badRequest("a script run has no retryable graph — re-fire its routine")
		}
		if run.Status == store.RunRunning {
			return badRequest("run is still running")
		}
		def := run.Graph.Node(body.Node)
		if def == nil {
			return badRequest("run has no node %q", body.Node)
		}
		nodes, err := tx.RunNodes(ctx, id)
		if err != nil {
			return err
		}
		var latest *store.RunNode
		for i := range nodes {
			n := &nodes[i]
			if n.NodeID == body.Node && (latest == nil || n.Iteration > latest.Iteration) {
				latest = n
			}
		}
		if latest == nil {
			return badRequest("node %s never ran in this run", body.Node)
		}
		if latest.Status != store.NodeFailed && latest.Status != store.NodeCancelled {
			return badRequest("node %s is %s; only failed or cancelled nodes retry", body.Node, latest.Status)
		}
		fresh := &store.RunNode{RunID: id, NodeID: body.Node, Iteration: latest.Iteration + 1, Type: def.Type, Status: store.NodeReady, Edges: map[string]string{"retry": "taken"}}
		if err := tx.CreateRunNode(ctx, fresh); err != nil {
			return err
		}
		if err := tx.SetWorkflowRunStatus(ctx, id, store.RunRunning); err != nil {
			return err
		}
		return tx.Journal(ctx, "workflow.run_retried", store.EntityWorkflow, id, map[string]any{"node": body.Node, "iteration": fresh.Iteration})
	})
	if err != nil {
		return 0, nil, err
	}
	s.KickFlow(ctx, id)
	s.log.InfoContext(ctx, "workflow run retried", "run_id", id, "node", body.Node)
	return http.StatusAccepted, map[string]string{"run_id": id, "status": "running"}, nil
}

// cancelWorkflowRun requests the whole run stop: waiting instances cancel
// now, running Works get the ordinary cancel request, and the engine settles
// the run to cancelled as they land.
func (s *Server) cancelWorkflowRun(r *http.Request) (int, any, error) {
	ctx := r.Context()
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		run, err := tx.GetWorkflowRun(ctx, id)
		if err != nil {
			return err
		}
		if run.Status != store.RunRunning {
			return badRequest("run is already %s", run.Status)
		}
		nodes, err := tx.RunNodes(ctx, id)
		if err != nil {
			return err
		}
		for i := range nodes {
			n := &nodes[i]
			switch n.Status {
			case store.NodePending, store.NodeReady:
				from := n.Status
				n.Status, n.Error, n.FinishedAt = store.NodeCancelled, "run cancelled", tx.Now()
				if err := tx.UpdateRunNodeFrom(ctx, n, from); err != nil {
					return err
				}
			case store.NodeRunning:
				if n.WorkID == "" {
					continue
				}
				if err := tx.CancelWork(ctx, n.WorkID, "human"); err != nil && !errors.Is(err, store.ErrConflict) {
					return err
				}
			}
		}
		return tx.Journal(ctx, "workflow.run_cancel_requested", store.EntityWorkflow, id, map[string]any{"workflow": run.WorkflowName})
	})
	if err != nil {
		return 0, nil, err
	}
	s.KickFlow(ctx, id)
	s.log.InfoContext(ctx, "workflow run cancel requested", "run_id", id)
	return http.StatusAccepted, map[string]string{"run_id": id, "status": "cancelling"}, nil
}

// workflowRun is one row of GET /api/v1/workflows/{name}/runs: the run's
// stored status and node instances.
type workflowRun struct {
	RunID      string           `json:"run_id"`
	State      string           `json:"state"`
	Trigger    string           `json:"trigger,omitempty"`
	CreatedAt  time.Time        `json:"created_at"`
	FinishedAt time.Time        `json:"finished_at,omitempty"`
	Nodes      []runNodeSummary `json:"nodes,omitempty"`
}

func (s *Server) workflowRuns(r *http.Request) (int, any, error) {
	ctx := r.Context()
	name := r.PathValue("name")
	if _, err := s.store.GetWorkflow(ctx, name); err != nil {
		return 0, nil, err
	}
	limit, err := listLimit(r)
	if err != nil {
		return 0, nil, err
	}
	runs, err := s.store.WorkflowRunsFor(ctx, name, limit)
	if err != nil {
		return 0, nil, err
	}
	out := make([]workflowRun, 0, len(runs))
	for _, run := range runs {
		nodes, err := s.store.RunNodes(ctx, run.ID)
		if err != nil {
			return 0, nil, err
		}
		row := workflowRun{RunID: run.ID, State: run.Status, Trigger: string(run.Trigger), CreatedAt: run.CreatedAt, FinishedAt: run.FinishedAt}
		for _, n := range nodes {
			row.Nodes = append(row.Nodes, runNodeSummary{NodeID: n.NodeID, Iteration: n.Iteration, Type: string(n.Type), Status: n.Status, WorkID: n.WorkID, Error: n.Error, StartedAt: n.StartedAt})
		}
		out = append(out, row)
	}
	sort.SliceStable(out, func(i, j int) bool { return out[i].CreatedAt.After(out[j].CreatedAt) })
	return http.StatusOK, out, nil
}

// workflowGenerations is GET /api/v1/workflows/{name}/generations — the
// changelog behind rollback: every generation with its source and snapshot.
func (s *Server) workflowGenerations(r *http.Request) (int, any, error) {
	rows, err := s.store.WorkflowGenerations(r.Context(), r.PathValue("name"), 50)
	if err != nil {
		return 0, nil, err
	}
	return http.StatusOK, rows, nil
}

// rollbackWorkflow is POST /api/v1/workflows/{name}/rollback {"generation":N}:
// restore that generation's snapshot as a NEW generation (sourced
// "rollback:<n>"), so the changelog keeps the full history — rollback is an
// audited forward step, never a rewrite.
func (s *Server) rollbackWorkflow(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	var req struct {
		Generation int `json:"generation"`
	}
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	if req.Generation <= 0 {
		return 0, nil, badRequest("generation is required: the generation to restore")
	}
	name := r.PathValue("name")
	snap, err := s.store.WorkflowAtGeneration(ctx, name, req.Generation)
	if err != nil {
		return 0, nil, err
	}
	var restored *store.Workflow
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		current, err := tx.GetWorkflow(ctx, name)
		if err != nil {
			return err
		}
		if current.Generation == req.Generation {
			return badRequest("workflow %s is already at generation %d", name, req.Generation)
		}
		wf := *snap
		wf.ID = current.ID
		if err := tx.UpdateWorkflowFrom(ctx, &wf, current.Generation, fmt.Sprintf("rollback:%d", req.Generation)); err != nil {
			return err
		}
		restored = &wf
		return tx.Journal(ctx, "workflow.rolled_back", store.EntityDaemon, wf.ID, map[string]any{
			"workflow": name, "restored_generation": req.Generation, "new_generation": wf.Generation})
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "workflow rolled back", "workflow", name, "restored", req.Generation, "generation", restored.Generation)
	return http.StatusOK, restored, nil
}
