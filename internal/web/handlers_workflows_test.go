package web

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// runDetail fetches GET /api/v1/workflow-runs/{id}.
func (h *harness) runDetail(runID string) workflowRunDetail {
	h.t.Helper()
	var out workflowRunDetail
	h.call(http.MethodGet, "/api/v1/workflow-runs/"+runID, nil, &out, http.StatusOK)
	return out
}

// nodeInstance plucks one instance from a run detail.
func nodeInstance(d workflowRunDetail, nodeID string, iter int) *runNodeDetail {
	for i := range d.Nodes {
		if d.Nodes[i].NodeID == nodeID && d.Nodes[i].Iteration == iter {
			return &d.Nodes[i]
		}
	}
	return nil
}

// completeNode claims the running node's work and completes it with the given
// state, which advances the run synchronously.
func (h *harness) completeNode(reqID string, state model.State) {
	h.t.Helper()
	c := h.mustClaim(reqID)
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 41)
	h.complete(c, completeRequest(state, time.Now()))
}

// A workflow of chained routines: create, run, and watch the engine
// materialize nodes stepwise — the dependant is never pre-created.
func TestWorkflowRunEngineChain(t *testing.T) {
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
	// The steps body is converted to the canonical graph: two routine nodes
	// chained by one success edge.
	if created.Generation != 1 || created.Graph == nil || len(created.Steps) != 0 {
		t.Fatalf("created = %+v", created)
	}
	if g := created.Graph; len(g.Nodes) != 2 || len(g.Edges) != 1 || g.Edges[0].From != "lint" || g.Edges[0].To != "fix" {
		t.Fatalf("graph = %+v", created.Graph)
	}

	var run workflowRunCreated
	h.call(http.MethodPost, "/api/v1/workflows/nightly/run", nil, &run, http.StatusCreated)
	if run.RunID == "" || run.Workflow != "nightly" {
		t.Fatalf("run = %+v", run)
	}

	// The synchronous advance materialized the root; the second node has no
	// instance yet.
	d := h.runDetail(run.RunID)
	if d.Status != store.RunRunning || d.Graph == nil {
		t.Fatalf("detail = %+v", d)
	}
	lint := nodeInstance(d, "lint", 1)
	if lint == nil || lint.Status != store.NodeRunning || lint.WorkID == "" {
		t.Fatalf("lint = %+v", lint)
	}
	if nodeInstance(d, "fix", 1) != nil {
		t.Fatal("fix pre-created; the engine should materialize it on readiness")
	}
	if lint.Work == nil || lint.Work.Work.WorkflowStep != "lint" || lint.Work.Work.Title != "nightly: lint" {
		t.Fatalf("lint work = %+v", lint.Work)
	}

	// Completing lint advances the run: fix materializes with a satisfied
	// blocked_by edge to lint's work.
	h.completeNode("wf-1", model.Succeeded)
	d = h.runDetail(run.RunID)
	if got := nodeInstance(d, "lint", 1); got.Status != store.NodeSucceeded {
		t.Fatalf("lint after complete = %+v", got)
	}
	fix := nodeInstance(d, "fix", 1)
	if fix == nil || fix.Status != store.NodeRunning || fix.WorkID == "" {
		t.Fatalf("fix = %+v", fix)
	}
	edges, err := h.st.DependencyEdges(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, e := range edges {
		if e.Work == fix.WorkID && e.BlockedBy == lint.WorkID && e.On == model.OnSuccess {
			found = true
		}
	}
	if !found {
		t.Fatalf("no blocked_by edge from fix to lint: %+v", edges)
	}

	h.completeNode("wf-2", model.Succeeded)
	d = h.runDetail(run.RunID)
	if d.Status != store.RunSucceeded || d.FinishedAt.IsZero() {
		t.Fatalf("final run = status %s finished %v", d.Status, d.FinishedAt)
	}

	// The runs listing carries the engine row with node summaries.
	var runs []workflowRun
	h.call(http.MethodGet, "/api/v1/workflows/nightly/runs", nil, &runs, http.StatusOK)
	if len(runs) != 1 || runs[0].RunID != run.RunID || runs[0].State != store.RunSucceeded || len(runs[0].Nodes) != 2 || runs[0].Legacy {
		t.Fatalf("runs = %+v", runs)
	}
}

// A failing node with no failure edge skips its dependants and fails the run
// — no Work is ever created for the skipped node.
func TestWorkflowRunFailureSkips(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("build")
	h.createRoutine("deploy")

	wf := store.Workflow{Name: "release", Steps: []store.WorkflowStep{
		{Name: "build", Routine: "build"},
		{Name: "deploy", Routine: "deploy"},
	}}
	h.call(http.MethodPost, "/api/v1/workflows", wf, nil, http.StatusCreated)
	var run workflowRunCreated
	h.call(http.MethodPost, "/api/v1/workflows/release/run", nil, &run, http.StatusCreated)
	h.completeNode("wf-f1", model.Failed)
	d := h.runDetail(run.RunID)
	if got := nodeInstance(d, "deploy", 1); got == nil || got.Status != store.NodeSkipped || got.WorkID != "" {
		t.Fatalf("deploy = %+v", got)
	}
	if d.Status != store.RunFailed {
		t.Fatalf("run = %s, want failed", d.Status)
	}
}

// A script node runs in the daemon between routine nodes, sees the upstream
// output, and its result feeds the switch that routes the run.
func TestWorkflowRunScriptAndSwitch(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("probe")
	h.createRoutine("docs")
	h.createRoutine("other")

	wf := store.Workflow{Name: "routed", Graph: &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{
			{ID: "probe", Type: store.NodeRoutine, Config: map[string]any{"routine": "probe"}},
			{ID: "shape", Type: store.NodeScript, Config: map[string]any{"source": "function main(input) { return {kind: input.steps.probe.status === 'succeeded' ? 'docs' : 'other'} }"}},
			{ID: "route", Type: store.NodeSwitch, Config: map[string]any{"expression": "input.steps.shape.output.kind"}},
			{ID: "docs", Type: store.NodeRoutine, Config: map[string]any{"routine": "docs"}},
			{ID: "other", Type: store.NodeRoutine, Config: map[string]any{"routine": "other"}},
		},
		Edges: []store.WorkflowGraphEdge{
			{From: "probe", To: "shape", When: store.WhenAlways},
			{From: "shape", To: "route"},
			{From: "route", To: "docs", When: store.WhenCase, Case: "docs"},
			{From: "route", To: "other", Default: true},
		},
	}}
	h.call(http.MethodPost, "/api/v1/workflows", wf, nil, http.StatusCreated)
	var run workflowRunCreated
	h.call(http.MethodPost, "/api/v1/workflows/routed/run", nil, &run, http.StatusCreated)
	h.completeNode("wf-s1", model.Succeeded)

	d := h.runDetail(run.RunID)
	if got := nodeInstance(d, "shape", 1); got == nil || got.Status != store.NodeSucceeded || !strings.Contains(string(got.Output), `"docs"`) {
		t.Fatalf("shape = %+v", got)
	}
	if got := nodeInstance(d, "route", 1); got == nil || got.Status != store.NodeSucceeded || !strings.Contains(string(got.Output), `"case":"docs"`) {
		t.Fatalf("route = %+v", got)
	}
	if got := nodeInstance(d, "docs", 1); got == nil || got.Status != store.NodeRunning {
		t.Fatalf("docs = %+v", got)
	}
	if got := nodeInstance(d, "other", 1); got == nil || got.Status != store.NodeSkipped {
		t.Fatalf("other = %+v", got)
	}
	if d.ScriptRuns != 2 {
		t.Errorf("script_runs = %d, want 2", d.ScriptRuns)
	}
}

// Cancelling a run cancels waiting instances immediately and requests cancel
// of running Works; the engine settles the run once they land.
func TestWorkflowRunCancel(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("slow")
	h.createRoutine("later")

	wf := store.Workflow{Name: "cancellable", Steps: []store.WorkflowStep{
		{Name: "slow", Routine: "slow"},
		{Name: "later", Routine: "later"},
	}}
	h.call(http.MethodPost, "/api/v1/workflows", wf, nil, http.StatusCreated)
	var run workflowRunCreated
	h.call(http.MethodPost, "/api/v1/workflows/cancellable/run", nil, &run, http.StatusCreated)
	h.call(http.MethodPost, "/api/v1/workflow-runs/"+run.RunID+"/cancel", nil, nil, http.StatusAccepted)
	// The root work was pending (unclaimed), so cancellation is immediate and
	// the engine settles the run to cancelled on the kick.
	d := h.runDetail(run.RunID)
	if got := nodeInstance(d, "slow", 1); got == nil || got.Status != store.NodeCancelled {
		t.Fatalf("slow = %+v", got)
	}
	if d.Status != store.RunCancelled {
		t.Fatalf("run = %s, want cancelled", d.Status)
	}
	// A second cancel is refused: the run is already terminal.
	if status, _ := h.do(http.MethodPost, "/api/v1/workflow-runs/"+run.RunID+"/cancel", nil, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("second cancel = %d", status)
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

	// A graph body with an uncapped loop edge is refused at definition time.
	loopy := store.Workflow{Name: "loopy", Graph: &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{
			{ID: "a", Type: store.NodeRoutine, Config: map[string]any{"routine": "real"}},
			{ID: "b", Type: store.NodeRoutine, Config: map[string]any{"routine": "real"}},
		},
		Edges: []store.WorkflowGraphEdge{{From: "a", To: "b"}, {From: "b", To: "a", Loop: true}},
	}}
	if status, body := h.do(http.MethodPost, "/api/v1/workflows", loopy, nil, ""); status != http.StatusBadRequest || !strings.Contains(string(body), "max_iterations") {
		t.Fatalf("uncapped loop = %d %s", status, body)
	}

	ok := store.Workflow{Name: "ok", Steps: []store.WorkflowStep{{Name: "a", Routine: "real"}}}
	h.call(http.MethodPost, "/api/v1/workflows", ok, &ok, http.StatusCreated)

	// Update needs the generation and bumps it; a stale one 409s.
	ok.Graph.Nodes = append(ok.Graph.Nodes, store.WorkflowNode{ID: "b", Type: store.NodeRoutine, Config: map[string]any{"routine": "real"}})
	var updated store.Workflow
	h.call(http.MethodPut, "/api/v1/workflows/ok?generation=1", ok, &updated, http.StatusOK)
	if updated.Generation != 2 {
		t.Fatalf("updated = %+v", updated)
	}
	if status, _ := h.do(http.MethodPut, "/api/v1/workflows/ok?generation=1", ok, nil, ""); status != http.StatusConflict {
		t.Fatalf("stale update = %d", status)
	}

	// The layout PATCH moves nodes without a generation bump.
	h.call(http.MethodPatch, "/api/v1/workflows/ok/layout", map[string]any{"positions": map[string]any{"a": map[string]float64{"x": 500, "y": 60}}}, nil, http.StatusNoContent)
	var after store.Workflow
	h.call(http.MethodGet, "/api/v1/workflows/ok", nil, &after, http.StatusOK)
	if after.Generation != 2 || after.Graph.Node("a").Position.X != 500 {
		t.Fatalf("after layout = gen %d pos %+v", after.Generation, after.Graph.Node("a").Position)
	}
	if status, body := h.do(http.MethodPatch, "/api/v1/workflows/ok/layout", map[string]any{"positions": map[string]any{"ghost": map[string]float64{"x": 1}}}, nil, ""); status != http.StatusNotFound {
		t.Fatalf("layout unknown node = %d %s", status, body)
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

// A routine node's result envelope `output` becomes the node output, and a
// downstream node's objective template reads it before its Work is created.
func TestWorkflowRunOutputPassing(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("probe")
	fixer := store.Routine{Name: "fixer", Mode: "run", Prompt: "do this: {{objective}}", Repositories: []string{"equitizr"}, Model: "haiku", TimeoutSeconds: 300, RequireSandbox: true}
	h.call(http.MethodPost, "/api/v1/routines", fixer, nil, http.StatusCreated)

	wf := store.Workflow{Name: "passing", Graph: &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{
			{ID: "probe", Type: store.NodeRoutine, Config: map[string]any{"routine": "probe"}},
			{ID: "fix", Type: store.NodeRoutine, Config: map[string]any{"routine": "fixer", "objective": "fix {{steps.probe.output.top}} ({{steps.probe.status}})"}},
		},
		Edges: []store.WorkflowGraphEdge{{From: "probe", To: "fix"}},
	}}
	h.call(http.MethodPost, "/api/v1/workflows", wf, nil, http.StatusCreated)
	var run workflowRunCreated
	h.call(http.MethodPost, "/api/v1/workflows/passing/run", nil, &run, http.StatusCreated)

	c := h.mustClaim("wf-o1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 41)
	req := completeRequest(model.Succeeded, time.Now())
	req.Result = json.RawMessage(`{"schema_version":1,"summary":"found the culprit","output":{"top":"parser"}}`)
	h.complete(c, req)

	d := h.runDetail(run.RunID)
	probe := nodeInstance(d, "probe", 1)
	if probe == nil || !strings.Contains(string(probe.Output), `"summary":"found the culprit"`) || !strings.Contains(string(probe.Output), `"top":"parser"`) {
		t.Fatalf("probe output = %s", probe.Output)
	}
	fix := nodeInstance(d, "fix", 1)
	if fix == nil || fix.Status != store.NodeRunning {
		t.Fatalf("fix = %+v", fix)
	}
	w, err := h.st.GetWork(context.Background(), fix.WorkID)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(w.Snapshot), "do this: fix parser (succeeded)") {
		t.Errorf("fix snapshot prompt = %s", w.Snapshot)
	}
}

// Retrying a failed run from its failed node re-opens it: a fresh instance
// materializes, and success re-fires the downstream wave.
func TestWorkflowRunRetry(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("build")
	h.createRoutine("deploy")

	wf := store.Workflow{Name: "retryable", Steps: []store.WorkflowStep{
		{Name: "build", Routine: "build"},
		{Name: "deploy", Routine: "deploy"},
	}}
	h.call(http.MethodPost, "/api/v1/workflows", wf, nil, http.StatusCreated)
	var run workflowRunCreated
	h.call(http.MethodPost, "/api/v1/workflows/retryable/run", nil, &run, http.StatusCreated)
	h.completeNode("wf-r1", model.Failed)
	if d := h.runDetail(run.RunID); d.Status != store.RunFailed {
		t.Fatalf("run = %s, want failed", d.Status)
	}

	// Retry of a node that did not fail is refused.
	if status, _ := h.do(http.MethodPost, "/api/v1/workflow-runs/"+run.RunID+"/retry", map[string]string{"node": "deploy"}, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("retry of skipped node = %d", status)
	}

	h.call(http.MethodPost, "/api/v1/workflow-runs/"+run.RunID+"/retry", map[string]string{"node": "build"}, nil, http.StatusAccepted)
	d := h.runDetail(run.RunID)
	if d.Status != store.RunRunning {
		t.Fatalf("run after retry = %s", d.Status)
	}
	b2 := nodeInstance(d, "build", 2)
	if b2 == nil || b2.Status != store.NodeRunning || b2.WorkID == "" {
		t.Fatalf("build#2 = %+v", b2)
	}
	h.completeNode("wf-r2", model.Succeeded)
	d = h.runDetail(run.RunID)
	if got := nodeInstance(d, "deploy", 2); got == nil || got.Status != store.NodeRunning {
		t.Fatalf("deploy#2 = %+v", got)
	}
	h.completeNode("wf-r3", model.Succeeded)
	if d = h.runDetail(run.RunID); d.Status != store.RunSucceeded {
		t.Fatalf("final = %s", d.Status)
	}
}

// A routine node whose Work cannot be created (unregistered repository) fails
// the node — with the reason on the instance — rather than wedging the run in
// a forever-retrying transaction.
func TestWorkflowRunMaterializationFailureFailsNode(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	ghost := store.Routine{Name: "ghostly", Mode: "run", Prompt: "p", Repositories: []string{"ghost-repo"}, Model: "haiku", TimeoutSeconds: 300}
	h.call(http.MethodPost, "/api/v1/routines", ghost, nil, http.StatusCreated)
	wf := store.Workflow{Name: "doomed", Steps: []store.WorkflowStep{{Name: "a", Routine: "ghostly"}}}
	h.call(http.MethodPost, "/api/v1/workflows", wf, nil, http.StatusCreated)
	var run workflowRunCreated
	h.call(http.MethodPost, "/api/v1/workflows/doomed/run", nil, &run, http.StatusCreated)
	d := h.runDetail(run.RunID)
	a := nodeInstance(d, "a", 1)
	if a == nil || a.Status != store.NodeFailed || !strings.Contains(a.Error, "not registered") {
		t.Fatalf("a = %+v", a)
	}
	if d.Status != store.RunFailed {
		t.Fatalf("run = %s, want failed", d.Status)
	}
}
