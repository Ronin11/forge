package web

import (
	"context"
	"net/http"
	"strings"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// A directive-target routine runs: the directive resolves (frontmatter mode/
// model/persona, includes expanded), the persona composes ahead, the
// objective injects, and the frozen snapshot carries the resolved content.
func TestDirectiveTargetRoutineRuns(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{
		"personas/reviewer.md":   "---\nmodel: haiku\n---\nYou are the reviewer.\n\n## mode: run\nRun teaching.",
		"directives/triage.md":   "---\nmode: run\npersona: reviewer\n---\nTriage {{repo}}: {{objective}}\n{{> checklist}}",
		"fragments/checklist.md": "- queue first",
	})

	target := store.Routine{Name: "nightly", Target: "directive:triage", Repositories: []string{"equitizr"}}
	h.call(http.MethodPost, "/api/v1/routines", target, &target, http.StatusCreated)

	var out workCreated
	h.call(http.MethodPost, "/api/v1/routines/nightly/run", map[string]string{"objective": "the gauges"}, &out, http.StatusCreated)
	snap := string(out.Work.Snapshot)
	for _, want := range []string{
		"You are the reviewer.", "Run teaching.", // persona + mode section
		"Triage {{repo}}: the gauges", // directive body, objective injected, {{repo}} left for claim
		"- queue first",               // include expanded
		`"model":"haiku"`,             // persona default model
		`"target":"directive:triage"`, // provenance in the frozen snapshot
	} {
		if !strings.Contains(snap, want) {
			t.Errorf("snapshot missing %q", want)
		}
	}
	if out.Work.Persona != "reviewer" || len(out.Work.Composition) == 0 {
		t.Errorf("work persona/composition: %+v", out.Work)
	}
	comp := string(out.Work.Composition)
	for _, frag := range []string{"triage", "checklist", "reviewer"} {
		if !strings.Contains(comp, `"name":"`+frag+`"`) {
			t.Errorf("composition missing fragment %q: %s", frag, comp)
		}
	}
}

// Definition-time refusals for target routines.
func TestTargetRoutineRefusals(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.withPrompts(map[string]string{"directives/real.md": "---\nmode: run\nmodel: haiku\n---\nbody"})
	for name, rt := range map[string]store.Routine{
		"target+content":    {Name: "x", Target: "directive:real", Prompt: "sneaky", Repositories: []string{"equitizr"}},
		"unknown directive": {Name: "x", Target: "directive:ghost", Repositories: []string{"equitizr"}},
		"unknown workflow":  {Name: "x", Target: "workflow:ghost", Repositories: []string{"equitizr"}},
		"bad target kind":   {Name: "x", Target: "prompt:real", Repositories: []string{"equitizr"}},
	} {
		if status, _ := h.do(http.MethodPost, "/api/v1/routines", rt, nil, ""); status != http.StatusBadRequest {
			t.Errorf("%s = %d, want 400", name, status)
		}
	}
	ok := store.Routine{Name: "ok", Target: "directive:real", Repositories: []string{"equitizr"}}
	h.call(http.MethodPost, "/api/v1/routines", ok, nil, http.StatusCreated)
}

// A workflow-target routine runs as a workflow run carrying the routine's
// repositories and objective as context; asking for a Work from it is a 400.
func TestWorkflowTargetRoutine(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{"directives/step.md": "---\nmode: run\nmodel: haiku\n---\nDo {{run.objective}}"})

	graph := map[string]any{
		"nodes": []map[string]any{{"id": "only", "type": "directive", "config": map[string]any{"directive": "step"}, "position": map[string]float64{"x": 0, "y": 0}}},
		"edges": []map[string]any{},
	}
	h.call(http.MethodPost, "/api/v1/workflows", map[string]any{"name": "wf-tgt", "graph": graph}, nil, http.StatusCreated)

	rt := store.Routine{Name: "wf-trigger", Target: "workflow:wf-tgt", Repositories: []string{"equitizr"}, Objective: "default obj"}
	h.call(http.MethodPost, "/api/v1/routines", rt, nil, http.StatusCreated)

	var created workflowRunCreated
	h.call(http.MethodPost, "/api/v1/routines/wf-trigger/run", map[string]string{"objective": "special obj"}, &created, http.StatusCreated)
	if created.Workflow != "wf-tgt" || created.RunID == "" {
		t.Fatalf("run = %+v", created)
	}
	run, err := h.st.GetWorkflowRun(context.Background(), created.RunID)
	if err != nil {
		t.Fatal(err)
	}
	if run.Context.Objective != "special obj" || len(run.Context.Repositories) != 1 || run.Context.Repositories[0] != "equitizr" {
		t.Errorf("run context = %+v", run.Context)
	}

	// As a Work: refused.
	if status, body := h.do(http.MethodPost, "/api/v1/work", map[string]string{"routine": "wf-trigger"}, nil, ""); status != http.StatusBadRequest || !strings.Contains(string(body), "workflow") {
		t.Errorf("work from workflow-target = %d %s", status, body)
	}
	// Preview: refused with a pointer.
	if status, _ := h.do(http.MethodGet, "/api/v1/routines/wf-trigger/preview", nil, nil, ""); status != http.StatusBadRequest {
		t.Errorf("preview of workflow-target = %d", status)
	}
}

// The scheduler fires a workflow-target routine as a run (trigger schedule,
// routine context), skips while a run is open, and reschedules.
func TestSchedulerFiresWorkflowTargetRoutine(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	ctx := context.Background()

	h.writeDirective("wfr-step", "---\nmode: run\nmodel: haiku\n---\np\n")
	graph := map[string]any{
		"nodes": []map[string]any{{"id": "only", "type": "directive", "config": map[string]any{"directive": "wfr-step"}, "position": map[string]float64{"x": 0, "y": 0}}},
		"edges": []map[string]any{},
	}
	h.call(http.MethodPost, "/api/v1/workflows", map[string]any{"name": "wfr", "graph": graph}, nil, http.StatusCreated)
	rt := store.Routine{Name: "wfr-cron", Target: "workflow:wfr", Repositories: []string{"equitizr"}, Objective: "scheduled obj",
		Schedule: "0 3 * * *", ScheduleEnabled: true}
	h.call(http.MethodPost, "/api/v1/routines", rt, &rt, http.StatusCreated)

	h.srv.scheduleTick(ctx) // backfill
	saved, err := h.st.GetRoutine(ctx, "wfr-cron")
	if err != nil || saved.NextDueAt.IsZero() {
		t.Fatalf("no backfill: %+v, %v", saved, err)
	}
	h.clock.Advance(saved.NextDueAt.Sub(h.clock.Now()) + time.Minute)
	h.srv.scheduleTick(ctx)

	runs, err := h.st.WorkflowRunsFor(ctx, "wfr", 10)
	if err != nil || len(runs) != 1 {
		t.Fatalf("runs = %d, %v", len(runs), err)
	}
	if runs[0].Trigger != model.TriggerSchedule || runs[0].Context.Objective != "scheduled obj" {
		t.Errorf("run = trigger %s context %+v", runs[0].Trigger, runs[0].Context)
	}

	// Open run → next occurrence skips.
	after, err := h.st.GetRoutine(ctx, "wfr-cron")
	if err != nil {
		t.Fatal(err)
	}
	h.clock.Advance(after.NextDueAt.Sub(h.clock.Now()) + time.Minute)
	h.srv.scheduleTick(ctx)
	if runs, err = h.st.WorkflowRunsFor(ctx, "wfr", 10); err != nil || len(runs) != 1 {
		t.Errorf("skip-if-running violated: %d runs, %v", len(runs), err)
	}
	final, err := h.st.GetRoutine(ctx, "wfr-cron")
	if err != nil || !final.NextDueAt.After(h.clock.Now()) {
		t.Errorf("not rescheduled: %v, %v", final.NextDueAt, err)
	}
}

// The scheduler fires a directive-target routine as ordinary Work through
// the same materialization the manual run uses.
func TestSchedulerFiresDirectiveTargetRoutine(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	ctx := context.Background()
	h.withPrompts(map[string]string{"directives/sweep.md": "---\nmode: run\nmodel: haiku\n---\nSweep: {{objective}}"})

	rt := store.Routine{Name: "sweep-cron", Target: "directive:sweep", Repositories: []string{"equitizr"},
		Objective: "the backlog", Schedule: "0 3 * * *", ScheduleEnabled: true}
	h.call(http.MethodPost, "/api/v1/routines", rt, &rt, http.StatusCreated)

	h.srv.scheduleTick(ctx)
	saved, err := h.st.GetRoutine(ctx, "sweep-cron")
	if err != nil {
		t.Fatal(err)
	}
	h.clock.Advance(saved.NextDueAt.Sub(h.clock.Now()) + time.Minute)
	h.srv.scheduleTick(ctx)

	works, err := h.st.OpenWork(ctx)
	if err != nil {
		t.Fatal(err)
	}
	var fired *store.Work
	for i := range works {
		if works[i].RoutineName == "sweep-cron" {
			fired = &works[i]
		}
	}
	if fired == nil || fired.Trigger != model.TriggerSchedule {
		t.Fatalf("fired = %+v", fired)
	}
	if !strings.Contains(string(fired.Snapshot), "Sweep: the backlog") {
		t.Errorf("snapshot did not materialize the directive with the routine objective: %s", fired.Snapshot)
	}
}

// The directive tester subject: POST /api/v1/directive-test {directive: name}
// composes the full assembly and records under directive:<name>.
func TestPromptTestDirectiveSubject(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{
		"personas/reviewer.md": "---\nmodel: haiku\n---\nYou review.",
		"directives/triage.md": "---\nmode: run\npersona: reviewer\n---\nTriage {{repo}}: {{objective}}",
	})
	var gotUser string
	h.srv.modelCall = func(_ context.Context, _, user, _ string) (string, error) {
		gotUser = user
		return "OK", nil
	}
	var out promptTestResponse
	h.call(http.MethodPost, "/api/v1/directive-test", map[string]string{"directive": "triage", "objective": "the gauges", "repo": "equitizr"}, &out, http.StatusOK)
	for _, want := range []string{"You review.", "Triage equitizr: the gauges"} {
		if !strings.Contains(gotUser, want) {
			t.Errorf("test prompt missing %q", want)
		}
	}
	if out.Model != "haiku" {
		t.Errorf("model = %q", out.Model)
	}
	var tests []store.PromptTest
	h.call(http.MethodGet, "/api/v1/directive-tests?subject=directive:triage", nil, &tests, http.StatusOK)
	if len(tests) != 1 || tests[0].Output != "OK" {
		t.Errorf("history = %+v", tests)
	}
}

// A two-directive graph runs end to end: each node materializes from the
// library (frontmatter mode/model, node envelope), and the objective template
// carries upstream output into the downstream directive node.
func TestWorkflowDirectiveNode(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("wfd-legacy")
	h.withPrompts(map[string]string{
		"directives/wfd-triage.md": "---\nmode: run\nmodel: sonnet\n---\nTriage next: {{objective}}",
	})

	graph := map[string]any{
		"nodes": []map[string]any{
			{"id": "old", "type": "directive", "config": map[string]any{"directive": "wfd-legacy"}, "position": map[string]float64{"x": 0, "y": 0}},
			{"id": "new", "type": "directive", "config": map[string]any{
				"directive": "wfd-triage", "objective": "follow up on {{steps.old.status}}",
				"timeout_seconds": 900, "max_turns": 7, "budget_class": "backlog",
			}, "position": map[string]float64{"x": 300, "y": 0}},
		},
		"edges": []map[string]any{{"from": "old", "to": "new", "when": "success"}},
	}
	h.call(http.MethodPost, "/api/v1/workflows", map[string]any{"name": "wfd", "graph": graph}, nil, http.StatusCreated)

	var run workflowRunCreated
	h.call(http.MethodPost, "/api/v1/workflows/wfd/run", map[string]any{"repositories": []string{"equitizr"}}, &run, http.StatusCreated)
	h.completeNode("wfd-1", model.Succeeded)

	d := h.runDetail(run.RunID)
	inst := nodeInstance(d, "new", 1)
	if inst == nil || inst.Status != store.NodeRunning || inst.WorkID == "" {
		t.Fatalf("directive node = %+v", inst)
	}
	w, err := h.st.GetWork(context.Background(), inst.WorkID)
	if err != nil {
		t.Fatal(err)
	}
	snap := string(w.Snapshot)
	for _, want := range []string{
		"Triage next: follow up on succeeded", // directive body + expanded template objective
		`"model":"sonnet"`,                    // frontmatter model
		`"timeout_seconds":900`,               // node envelope
		`"max_turns":7`,
		`"budget_class":"backlog"`,
		`"target":"directive:wfd-triage"`,
	} {
		if !strings.Contains(snap, want) {
			t.Errorf("snapshot missing %q\n%s", want, snap)
		}
	}
	if w.RoutineName != "wfd-triage" || w.BudgetClass != "backlog" {
		t.Errorf("work = routine %q class %q", w.RoutineName, w.BudgetClass)
	}

	h.completeNode("wfd-2", model.Succeeded)
	if d = h.runDetail(run.RunID); d.Status != store.RunSucceeded {
		t.Fatalf("run = %s", d.Status)
	}
}

// Save-time validation for directive nodes: unknown directives and bad
// envelopes are 400s at save, not runtime failures.
func TestWorkflowDirectiveNodeValidation(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.withPrompts(map[string]string{"directives/real.md": "---\nmode: run\nmodel: haiku\n---\nbody"})
	node := func(cfg map[string]any) map[string]any {
		return map[string]any{
			"name": "wfv",
			"graph": map[string]any{
				"nodes": []map[string]any{{"id": "n", "type": "directive", "config": cfg, "position": map[string]float64{"x": 0, "y": 0}}},
				"edges": []map[string]any{},
			},
		}
	}
	for name, cfg := range map[string]map[string]any{
		"unknown directive": {"directive": "ghost"},
		"no directive":      {},
		"bad timeout":       {"directive": "real", "timeout_seconds": 999999},
		"bad class":         {"directive": "real", "budget_class": "platinum"},
	} {
		if status, _ := h.do(http.MethodPost, "/api/v1/workflows", node(cfg), nil, ""); status != http.StatusBadRequest {
			t.Errorf("%s = %d, want 400", name, status)
		}
	}
	h.call(http.MethodPost, "/api/v1/workflows", node(map[string]any{"directive": "real"}), nil, http.StatusCreated)
}
