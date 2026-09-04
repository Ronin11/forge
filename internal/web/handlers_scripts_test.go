package web

import (
	"context"
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
