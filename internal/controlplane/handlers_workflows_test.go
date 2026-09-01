package controlplane

import (
	"context"
	"net/http"
	"strings"
	"testing"

	"forge/internal/core/model"
	"forge/internal/store"
)

// A workflow of chained routines: create, run, and watch the second step wait
// for the first through the ordinary dependency machinery.
func TestWorkflowRunInstantiatesChain(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("lint-all")
	h.createRoutine("fix-lint")

	wf := store.Workflow{Name: "nightly", Steps: []store.WorkflowStep{
		{Name: "lint", Routine: "lint-all"},
		{Name: "fix", Routine: "fix-lint"},
	}}
	var created store.Workflow
	h.call(http.MethodPost, "/api/v1/workflows", wf, &created, http.StatusCreated)
	if created.Generation != 1 || len(created.Steps) != 2 || len(created.Steps[1].After) != 1 {
		t.Fatalf("created = %+v", created)
	}

	var run workflowRunCreated
	h.call(http.MethodPost, "/api/v1/workflows/nightly/run", nil, &run, http.StatusCreated)
	if len(run.Works) != 2 || run.RunID == "" {
		t.Fatalf("run = %+v", run)
	}
	first, second := run.Works[0].Work, run.Works[1].Work
	if first.WorkflowRunID != run.RunID || first.WorkflowName != "nightly" || first.WorkflowStep != "lint" || second.WorkflowStep != "fix" {
		t.Errorf("stamps: first = %+v second = %+v", first, second)
	}
	if first.Title != "nightly: lint" {
		t.Errorf("title = %q", first.Title)
	}
	edges, err := h.st.DependencyEdges(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, e := range edges {
		if e.Work == second.ID && e.BlockedBy == first.ID && e.On == model.OnSuccess {
			found = true
		}
	}
	if !found {
		t.Fatalf("no blocked_by edge from fix to lint: %+v", edges)
	}

	// The queue admits only the first step; the claim proves it.
	c := h.mustClaim("wf-1")
	if c.WorkID != first.ID {
		t.Fatalf("claim = %+v, want the lint step", c)
	}
	if status, _ := h.claim(testWorkerID, "wf-2"); status != http.StatusNoContent {
		t.Fatalf("fix step claimable while lint runs: %d", status)
	}

	// The runs listing groups by run id and derives a state.
	var runs []workflowRun
	h.call(http.MethodGet, "/api/v1/workflows/nightly/runs", nil, &runs, http.StatusOK)
	if len(runs) != 1 || runs[0].RunID != run.RunID || len(runs[0].Works) != 2 {
		t.Fatalf("runs = %+v", runs)
	}
	if runs[0].State != "running" {
		t.Errorf("run state = %q, want running", runs[0].State)
	}
	if runs[0].Works[0].Work.WorkflowStep != "lint" {
		t.Errorf("step order = %+v", runs[0].Works)
	}
}

// Definition-time refusals: unknown or archived routines, forward references,
// stale generations, archived workflows.
func TestWorkflowValidationAndLifecycle(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.createRoutine("real")

	bad := store.Workflow{Name: "bad", Steps: []store.WorkflowStep{{Name: "a", Routine: "ghost"}}}
	status, body := h.do(http.MethodPost, "/api/v1/workflows", bad, nil, "")
	if status != http.StatusBadRequest || !strings.Contains(string(body), "does not exist") {
		t.Fatalf("unknown routine = %d %s", status, body)
	}

	fwd := store.Workflow{Name: "fwd", Steps: []store.WorkflowStep{
		{Name: "a", Routine: "real", After: []store.WorkflowEdge{{Step: "b"}}},
		{Name: "b", Routine: "real"},
	}}
	if status, body := h.do(http.MethodPost, "/api/v1/workflows", fwd, nil, ""); status != http.StatusBadRequest || !strings.Contains(string(body), "earlier step") {
		t.Fatalf("forward reference = %d %s", status, body)
	}

	ok := store.Workflow{Name: "ok", Steps: []store.WorkflowStep{{Name: "a", Routine: "real"}}}
	h.call(http.MethodPost, "/api/v1/workflows", ok, &ok, http.StatusCreated)

	// Update needs the generation and bumps it; a stale one 409s.
	ok.Steps = append(ok.Steps, store.WorkflowStep{Name: "b", Routine: "real"})
	var updated store.Workflow
	h.call(http.MethodPut, "/api/v1/workflows/ok?generation=1", ok, &updated, http.StatusOK)
	if updated.Generation != 2 {
		t.Fatalf("updated = %+v", updated)
	}
	if status, _ := h.do(http.MethodPut, "/api/v1/workflows/ok?generation=1", ok, nil, ""); status != http.StatusConflict {
		t.Fatalf("stale update = %d", status)
	}

	// Archive hides it from the list and refuses runs.
	h.call(http.MethodDelete, "/api/v1/workflows/ok", nil, nil, http.StatusNoContent)
	var listed []store.Workflow
	h.call(http.MethodGet, "/api/v1/workflows", nil, &listed, http.StatusOK)
	if len(listed) != 0 {
		t.Fatalf("archived workflow listed: %+v", listed)
	}
	if status, body := h.do(http.MethodPost, "/api/v1/workflows/ok/run", nil, nil, ""); status != http.StatusBadRequest || !strings.Contains(string(body), "archived") {
		t.Fatalf("archived run = %d %s", status, body)
	}
	if status, _ := h.do(http.MethodPost, "/api/v1/workflows/missing/run", nil, nil, ""); status != http.StatusNotFound {
		t.Fatalf("missing run = %d", status)
	}
}
