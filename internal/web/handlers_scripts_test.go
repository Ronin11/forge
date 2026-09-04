package web

import (
	"context"
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// A named script node resolves from the library at execution: params ride
// into input.params, the header timeout applies, and the whole run completes
// without any agent involvement.
func TestWorkflowNamedScriptNode(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{
		"scripts/doubler.js": "/**forge\n * description: doubles n\n */\nfunction main(input) { return {doubled: input.params.n * 2, obj: input.run.objective} }",
	})
	graph := map[string]any{
		"nodes": []map[string]any{
			{"id": "calc", "type": "script", "config": map[string]any{"script": "doubler", "params": map[string]any{"n": 21}}, "position": map[string]float64{"x": 0, "y": 0}},
		},
		"edges": []map[string]any{},
	}
	h.call(http.MethodPost, "/api/v1/workflows", map[string]any{"name": "calcflow", "graph": graph}, nil, http.StatusCreated)
	var run workflowRunCreated
	h.call(http.MethodPost, "/api/v1/workflows/calcflow/run", map[string]any{"objective": "double it"}, &run, http.StatusCreated)

	d := h.runDetail(run.RunID)
	if d.Status != store.RunSucceeded {
		t.Fatalf("run = %s", d.Status)
	}
	inst := nodeInstance(d, "calc", 1)
	if inst == nil || !strings.Contains(string(inst.Output), `"doubled":42`) || !strings.Contains(string(inst.Output), `"obj":"double it"`) {
		t.Fatalf("output = %s", inst.Output)
	}
}

// Save-time refusals for script nodes, and a renamed script failing cleanly
// at execution.
func TestWorkflowNamedScriptValidation(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{"scripts/real.js": "function main(i){return 1}"})
	node := func(cfg map[string]any) map[string]any {
		return map[string]any{"name": "sv", "graph": map[string]any{
			"nodes": []map[string]any{{"id": "n", "type": "script", "config": cfg, "position": map[string]float64{"x": 0, "y": 0}}},
			"edges": []map[string]any{},
		}}
	}
	for name, cfg := range map[string]map[string]any{
		"neither":        {},
		"both":           {"script": "real", "source": "function main(i){return 1}"},
		"unknown script": {"script": "ghost"},
		"bad name":       {"script": "Bad Name"},
	} {
		if status, _ := h.do(http.MethodPost, "/api/v1/workflows", node(cfg), nil, ""); status != http.StatusBadRequest {
			t.Errorf("%s = %d, want 400", name, status)
		}
	}
	h.call(http.MethodPost, "/api/v1/workflows", node(map[string]any{"script": "real"}), nil, http.StatusCreated)

	// Delete the script after save: the node fails cleanly at execution and
	// the run fails — no wedge.
	if err := os.Remove(filepath.Join(h.libDir, "scripts", "real.js")); err != nil {
		t.Fatal(err)
	}
	if err := h.srv.promptsReload(); err != nil {
		t.Fatal(err)
	}
	var run workflowRunCreated
	h.call(http.MethodPost, "/api/v1/workflows/sv/run", nil, &run, http.StatusCreated)
	d := h.runDetail(run.RunID)
	if d.Status != store.RunFailed {
		t.Fatalf("deleted-script run = %s", d.Status)
	}
	if inst := nodeInstance(d, "n", 1); inst == nil || !strings.Contains(inst.Error, "not in the library") {
		t.Fatalf("node = %+v", inst)
	}
}

// A script-target routine fires as a synthetic run (manual + scheduled),
// skips while one is open, and refuses retry.
func TestScriptTriggerRoutine(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	ctx := context.Background()
	h.withPrompts(map[string]string{
		"scripts/sweeper.js": "function main(input) { return {swept: input.run.objective} }",
	})
	rt := store.Routine{Name: "sweep-cron", Target: "script:sweeper", Objective: "the yard",
		Repositories: []string{"equitizr"}, Schedule: "0 3 * * *", ScheduleEnabled: true}
	h.call(http.MethodPost, "/api/v1/routines", rt, &rt, http.StatusCreated)

	// Manual fire.
	var created workflowRunCreated
	h.call(http.MethodPost, "/api/v1/routines/sweep-cron/run", map[string]string{"objective": "override"}, &created, http.StatusCreated)
	if created.Workflow != "script:sweeper" || created.RunID == "" {
		t.Fatalf("created = %+v", created)
	}
	run, err := h.st.GetWorkflowRun(ctx, created.RunID)
	if err != nil || run.Status != store.RunSucceeded || run.Context.Objective != "override" {
		t.Fatalf("run = %+v, %v", run, err)
	}
	// Retry is refused with a pointer.
	if status, body := h.do(http.MethodPost, "/api/v1/workflow-runs/"+created.RunID+"/retry", map[string]string{"node": "main"}, nil, ""); status != http.StatusBadRequest || !strings.Contains(string(body), "re-fire") {
		t.Errorf("retry = %d %s", status, body)
	}

	// Scheduled fire uses the routine's own objective.
	h.srv.scheduleTick(ctx)
	saved, err := h.st.GetRoutine(ctx, "sweep-cron")
	if err != nil {
		t.Fatal(err)
	}
	h.clock.Advance(saved.NextDueAt.Sub(h.clock.Now()) + time.Minute)
	h.srv.scheduleTick(ctx)
	runs, err := h.st.WorkflowRunsFor(ctx, "script:sweeper", 10)
	if err != nil || len(runs) != 2 {
		t.Fatalf("runs = %d, %v", len(runs), err)
	}
	var scheduled *store.WorkflowRun
	for i := range runs {
		if runs[i].Trigger == model.TriggerSchedule {
			scheduled = &runs[i]
		}
	}
	if scheduled == nil || scheduled.Context.Objective != "the yard" {
		t.Fatalf("scheduled run = %+v", scheduled)
	}

	// An unknown-script trigger is refused at save.
	bad := store.Routine{Name: "ghost-cron", Target: "script:ghost", Repositories: []string{"equitizr"}}
	if status, _ := h.do(http.MethodPost, "/api/v1/routines", bad, nil, ""); status != http.StatusBadRequest {
		t.Errorf("unknown script trigger = %d", status)
	}
}

// The library search API merges workflow rows with library hits under one
// ranking, filters kinds, and script-test runs the sandbox.
func TestLibrarySearchAndScriptTest(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{
		"directives/triage-repo.md": "---\nmode: run\nmodel: haiku\ndescription: sweep a repository\ntool: true\n---\nSurvey.",
		"scripts/rank.js":           "/**forge\n * description: rank triage output\n * input: {\"type\":\"object\"}\n * tool: true\n * timeout_ms: 2000\n */\nfunction main(i){ return {got: i.params} }",
	})
	graph := map[string]any{"nodes": []map[string]any{{"id": "n", "type": "directive", "config": map[string]any{"directive": "triage-repo"}, "position": map[string]float64{"x": 0, "y": 0}}}, "edges": []map[string]any{}}
	h.call(http.MethodPost, "/api/v1/workflows", map[string]any{"name": "triage-flow", "description": "triage then report", "tool": true, "graph": graph}, nil, http.StatusCreated)

	var out struct {
		Hits []struct {
			Name string `json:"name"`
			Kind string `json:"kind"`
			Tool bool   `json:"tool"`
		} `json:"hits"`
	}
	h.call(http.MethodGet, "/api/v1/library/search?q=triage", nil, &out, http.StatusOK)
	kinds := map[string]string{}
	for _, hit := range out.Hits {
		kinds[hit.Name] = hit.Kind
		if !hit.Tool {
			t.Errorf("%s should be tool-flagged", hit.Name)
		}
	}
	if kinds["triage-repo"] != "directive" || kinds["rank"] != "script" || kinds["triage-flow"] != "workflow" {
		t.Fatalf("hits = %+v", out.Hits)
	}
	h.call(http.MethodGet, "/api/v1/library/search?q=triage&kind=workflow", nil, &out, http.StatusOK)
	if len(out.Hits) != 1 || out.Hits[0].Name != "triage-flow" {
		t.Errorf("kind filter = %+v", out.Hits)
	}

	// script-test: output, throw, timeout.
	var res struct {
		Output    json.RawMessage `json:"output"`
		Error     string          `json:"error"`
		ElapsedMS int64           `json:"elapsed_ms"`
	}
	h.call(http.MethodPost, "/api/v1/script-test", map[string]any{"name": "rank", "input": map[string]int{"n": 7}}, &res, http.StatusOK)
	if !strings.Contains(string(res.Output), `"n":7`) || res.Error != "" {
		t.Errorf("script-test = %+v", res)
	}
	h.call(http.MethodPost, "/api/v1/script-test", map[string]any{"source": "function main(i){ throw new Error('boom') }"}, &res, http.StatusOK)
	if !strings.Contains(res.Error, "boom") {
		t.Errorf("throw = %+v", res)
	}
	if status, _ := h.do(http.MethodPost, "/api/v1/script-test", map[string]any{"name": "ghost"}, nil, ""); status != http.StatusBadRequest {
		t.Errorf("unknown script = %d", status)
	}
}
