package controlplane

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

// decodeWorkflow reads a workflow body and rejects what the API decides before
// the store does: the name.
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

// checkStepRoutines refuses a workflow whose steps name a routine that does not
// exist or is archived — at definition time, where the mistake is cheap.
func checkStepRoutines(ctx context.Context, tx *store.Tx, wf *store.Workflow) error {
	for _, st := range wf.Steps {
		rt, err := tx.GetRoutine(ctx, st.Routine)
		if err != nil {
			return badRequest("step %s: routine %s does not exist", st.Name, st.Routine)
		}
		if !rt.ArchivedAt.IsZero() {
			return badRequest("step %s: routine %s is archived", st.Name, st.Routine)
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
	s.log.InfoContext(ctx, "workflow created", "workflow", wf.Name, "workflow_id", wf.ID, "steps", len(wf.Steps))
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

// runWorkflow instantiates every step as a Work in one transaction: either the
// whole run is admitted or none of it. Step edges become work_dependencies, so
// a later step sits blocked until what it runs after is done.
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
		workOf := map[string]string{} // step name → work id, filled in step order
		for _, st := range wf.Steps {
			var edges []model.Edge
			for _, e := range st.After {
				on := e.On
				if on == "" {
					on = model.OnSuccess
				}
				edges = append(edges, model.Edge{BlockedBy: workOf[e.Step], On: on, StackOn: e.StackOn})
			}
			created, err := s.createWorkTx(ctx, tx, workRequest{
				Routine:       st.Routine,
				Repositories:  body.Repositories,
				Objective:     body.Objective,
				Title:         wf.Name + ": " + st.Name,
				workflowRunID: out.RunID, workflowName: wf.Name, workflowStep: st.Name,
				stepEdges: edges,
			})
			if err != nil {
				return err
			}
			workOf[st.Name] = created.Work.ID
			out.Works = append(out.Works, created)
		}
		return tx.Journal(ctx, "workflow.run_created", store.EntityWork, out.RunID, map[string]any{"workflow": wf.Name, "generation": wf.Generation, "steps": len(wf.Steps)})
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "workflow run created", "workflow", name, "run_id", out.RunID, "works", len(out.Works))
	return http.StatusCreated, out, nil
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
