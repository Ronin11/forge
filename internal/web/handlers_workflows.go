package web

import (
	"context"
	"net/http"
	"slices"
	"strconv"
	"time"

	"forge/internal/core/engine"
	"forge/internal/core/model"
	"forge/internal/core/store"
)

// Workflows are routines strung together (store.Workflow). Running one
// instantiates one Work per step with blocked_by edges inside a single
// transaction; from there the queue, dependency, and attention machinery treat
// the steps like any other Work. A run has no state row — GET .../runs derives
// it from the Works stamped with the run id.

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
	if err := wf.Normalize(); err != nil {
		return nil, badRequest("%v", err)
	}
	return &wf, nil
}

// checkStepRoutines refuses a workflow whose routine nodes name a routine that
// does not exist or is archived — at definition time, where the mistake is
// cheap. Legacy `steps` bodies are converted by the store before this runs, so
// the check normalizes first to see the graph either way.
func checkStepRoutines(ctx context.Context, tx *store.Tx, wf *store.Workflow) error {
	if wf.Graph == nil {
		return nil // normalize in the store surfaces the real validation error
	}
	for _, n := range wf.Graph.Nodes {
		if n.Type != store.NodeRoutine {
			continue
		}
		cfg, err := n.RoutineConfig()
		if err != nil {
			return badRequest("%v", err)
		}
		rt, err := tx.GetRoutine(ctx, cfg.Routine)
		if err != nil {
			return badRequest("node %s: routine %s does not exist", n.ID, cfg.Routine)
		}
		if !rt.ArchivedAt.IsZero() {
			return badRequest("node %s: routine %s is archived", n.ID, cfg.Routine)
		}
	}
	return nil
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
		if err := checkStepRoutines(ctx, tx, wf); err != nil {
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
		if err := checkStepRoutines(ctx, tx, wf); err != nil {
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

// workflowRunCreated is POST /api/v1/workflows/{name}/run's 201 body.
type workflowRunCreated struct {
	RunID    string        `json:"run_id"`
	Workflow string        `json:"workflow"`
	Works    []workCreated `json:"works"`
}

// runWorkflow instantiates a static graph — routine nodes with
// success/always edges, no loops — as Works in one transaction: either the
// whole run is admitted or none of it. Edges become work_dependencies, so a
// node sits blocked until what it runs after is done. Graphs that need
// runtime evaluation (scripts, switches, failure edges, loops) go through the
// run engine instead (B2 of the revamp; refused until it lands).
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
	out := workflowRunCreated{RunID: model.NewID(), Workflow: name, Works: []workCreated{}}
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		wf, err := tx.GetWorkflow(ctx, name)
		if err != nil {
			return err
		}
		if !wf.ArchivedAt.IsZero() {
			return badRequest("workflow %s is archived", wf.Name)
		}
		order, err := staticOrder(wf.Graph)
		if err != nil {
			return err
		}
		workOf := map[string]string{} // node id → work id, filled in topo order
		for _, id := range order {
			n := wf.Graph.Node(id)
			cfg, err := n.RoutineConfig()
			if err != nil {
				return badRequest("%v", err)
			}
			var edges []model.Edge
			for _, e := range wf.Graph.Edges {
				if e.To != n.ID {
					continue
				}
				on := model.OnSuccess
				if e.When == store.WhenAlways {
					on = model.OnTerminal
				}
				edges = append(edges, model.Edge{BlockedBy: workOf[e.From], On: on, StackOn: e.StackOn})
			}
			repos := body.Repositories
			if len(cfg.Repositories) > 0 {
				repos = cfg.Repositories
			}
			objective := body.Objective
			if cfg.Objective != "" {
				objective = cfg.Objective
			}
			created, err := s.createWorkTx(ctx, tx, workRequest{
				Routine:       cfg.Routine,
				Repositories:  repos,
				Objective:     objective,
				Title:         wf.Name + ": " + n.ID,
				workflowRunID: out.RunID, workflowName: wf.Name, workflowStep: n.ID,
				stepEdges: edges,
			})
			if err != nil {
				return err
			}
			workOf[n.ID] = created.Work.ID
			out.Works = append(out.Works, created)
		}
		return tx.Journal(ctx, "workflow.run_created", store.EntityWork, out.RunID, map[string]any{"workflow": wf.Name, "generation": wf.Generation, "nodes": len(order)})
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "workflow run created", "workflow", name, "run_id", out.RunID, "works", len(out.Works))
	return http.StatusCreated, out, nil
}

// staticOrder returns the instantiation order for a graph the static run path
// can execute: routine nodes joined by success/always edges. Anything needing
// runtime evaluation is refused with a pointer at what.
func staticOrder(g *store.WorkflowGraph) ([]string, error) {
	for _, n := range g.Nodes {
		if n.Type != store.NodeRoutine {
			return nil, badRequest("node %s: %s nodes need the run engine, which this Forge does not have yet", n.ID, n.Type)
		}
	}
	for _, e := range g.Edges {
		if e.Loop {
			return nil, badRequest("loop edge %s→%s needs the run engine, which this Forge does not have yet", e.From, e.To)
		}
		if e.When == store.WhenFailure || e.When == store.WhenCase || e.Default {
			return nil, badRequest("edge %s→%s (%s) needs the run engine, which this Forge does not have yet", e.From, e.To, e.When)
		}
	}
	order, err := g.TopoOrder()
	if err != nil {
		return nil, badRequest("%v", err)
	}
	return order, nil
}

// workflowRun is one row of GET /api/v1/workflows/{name}/runs: the Works one
// run instantiated, with their derived states (targets only — dependency and
// budget effects show in the queue) and a coarse aggregate.
type workflowRun struct {
	RunID     string        `json:"run_id"`
	State     string        `json:"state"`
	CreatedAt time.Time     `json:"created_at"`
	Works     []workSummary `json:"works"`
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
	work, err := s.store.WorkForWorkflow(ctx, name, limit)
	if err != nil {
		return 0, nil, err
	}
	ids := make([]string, len(work))
	for i, wk := range work {
		ids[i] = wk.ID
	}
	targets, err := s.store.TargetsForWorks(ctx, ids)
	if err != nil {
		return 0, nil, err
	}
	byRun := map[string]*workflowRun{}
	order := []string{}
	for _, wk := range work {
		run, ok := byRun[wk.WorkflowRunID]
		if !ok {
			run = &workflowRun{RunID: wk.WorkflowRunID}
			byRun[wk.WorkflowRunID] = run
			order = append(order, wk.WorkflowRunID)
		}
		ts := targets[wk.ID]
		if ts == nil {
			ts = []store.Target{}
		}
		state := model.DeriveWorkState(model.WorkInputs{Targets: engine.TargetStates(ts), Integrate: wk.Integrate})
		run.Works = append(run.Works, workSummary{Work: wk, State: state, Targets: ts})
	}
	out := make([]workflowRun, 0, len(byRun))
	for _, id := range order {
		run := byRun[id]
		// The query is newest-first, so a run's Works arrived in reverse
		// instantiation order; flip them back to step order.
		slices.Reverse(run.Works)
		run.CreatedAt = run.Works[0].Work.CreatedAt
		run.State = aggregateRunState(run.Works)
		out = append(out, *run)
	}
	return http.StatusOK, out, nil
}

// aggregateRunState is the coarse one-word summary of a run: what a list row
// shows. It is derived, never stored.
func aggregateRunState(works []workSummary) string {
	terminal, succeeded, cancelled, failed := true, true, true, true
	attention, running := false, false
	for _, w := range works {
		switch w.State {
		case model.WorkSucceeded, model.WorkMerged:
			cancelled, failed = false, false
		case model.WorkCancelled:
			succeeded, failed = false, false
		case model.WorkFailed, model.WorkUnverified, model.WorkPartial:
			succeeded, cancelled = false, false
		default:
			terminal = false
			if w.State == model.WorkWaitingHuman || w.State == model.WorkConflict {
				attention = true
			}
			if w.State == model.WorkRunning || w.State == model.WorkMerging {
				running = true
			}
		}
	}
	switch {
	case terminal && succeeded:
		return "succeeded"
	case terminal && cancelled:
		return "cancelled"
	case terminal && failed:
		return "failed"
	case terminal:
		return "partial"
	case attention:
		return "attention"
	case running:
		return "running"
	}
	return "pending"
}
