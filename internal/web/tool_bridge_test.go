package web

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
	"testing"

	"forge/internal/core/model"
)

// bridgeTool calls one daemon tool the way forge mcp does, as the given
// attempt over the open unix transport.
func (h *harness) bridgeTool(attemptID, name string, input any) (int, []byte) {
	h.t.Helper()
	raw, err := json.Marshal(input)
	if err != nil {
		h.t.Fatal(err)
	}
	return h.do(http.MethodPost, "/api/v1/tools/"+name,
		map[string]any{"schema_version": 1, "attempt_id": attemptID, "input": json.RawMessage(raw)}, nil, "")
}

// The tool/skill bridge end to end: an agent discovers library content,
// runs a tool-flagged script, spawns a directive sub-task with provenance
// stamped, and fires a tool-flagged workflow — with every guardrail
// asserted.
func TestToolBridge(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{
		"directives/spawnable.md":  "---\nmode: run\nmodel: haiku\ndescription: a spawnable task\ntool: true\n---\nDo: {{objective}}",
		"directives/not-a-tool.md": "---\nmode: run\nmodel: haiku\n---\nprivate",
		"scripts/adder.js":         "/**forge\n * description: adds a and b\n * input: {\"type\":\"object\"}\n * tool: true\n */\nfunction main(i){ return {sum: i.params.a + i.params.b} }",
		"scripts/private.js":       "function main(i){ return 1 }",
	})
	graph := map[string]any{"nodes": []map[string]any{{"id": "n", "type": "directive", "config": map[string]any{"directive": "spawnable"}, "position": map[string]float64{"x": 0, "y": 0}}}, "edges": []map[string]any{}}
	h.call(http.MethodPost, "/api/v1/workflows", map[string]any{"name": "callable-flow", "description": "fireable", "tool": true, "graph": graph}, nil, http.StatusCreated)
	h.call(http.MethodPost, "/api/v1/workflows", map[string]any{"name": "private-flow", "graph": graph}, nil, http.StatusCreated)

	// A real attempt to call from.
	h.createRoutine("caller")
	h.run("caller")
	claim := h.mustClaim("bridge-1")

	// forge_library: search sees kinds + tool flags; get returns full content.
	status, body := h.bridgeTool(claim.AttemptID, "forge_library", map[string]any{"query": "spawnable"})
	if status != http.StatusOK || !strings.Contains(string(body), `"kind":"directive"`) || !strings.Contains(string(body), `"tool":true`) {
		t.Fatalf("library search = %d %s", status, body)
	}
	status, body = h.bridgeTool(claim.AttemptID, "forge_library", map[string]any{"name": "spawnable", "kind": "directive"})
	if status != http.StatusOK || !strings.Contains(string(body), "Do: {{objective}}") {
		t.Fatalf("library get = %d %s", status, body)
	}

	// forge_script_run: output for tool-flagged, 400 for private/unknown.
	status, body = h.bridgeTool(claim.AttemptID, "forge_script_run", map[string]any{"script": "adder", "input": map[string]int{"a": 2, "b": 3}})
	if status != http.StatusOK || !strings.Contains(string(body), `"sum":5`) {
		t.Fatalf("script run = %d %s", status, body)
	}
	if status, body = h.bridgeTool(claim.AttemptID, "forge_script_run", map[string]any{"script": "private"}); status != http.StatusBadRequest || !strings.Contains(string(body), "not tool-flagged") {
		t.Errorf("private script = %d %s", status, body)
	}

	// forge_directive_run: spawns with provenance; refusals for flag/objective.
	status, body = h.bridgeTool(claim.AttemptID, "forge_directive_run", map[string]any{"directive": "spawnable", "objective": "sub-task one"})
	if status != http.StatusOK {
		t.Fatalf("directive run = %d %s", status, body)
	}
	var spawned struct {
		Output struct {
			WorkID string `json:"work_id"`
		} `json:"output"`
	}
	if err := json.Unmarshal(body, &spawned); err != nil || spawned.Output.WorkID == "" {
		t.Fatalf("spawn body = %s", body)
	}
	child, err := h.st.GetWork(context.Background(), spawned.Output.WorkID)
	if err != nil {
		t.Fatal(err)
	}
	if child.Cause != model.CauseTool || child.SubmittedBy != "agent:"+claim.AttemptID || child.BudgetClass != model.ClassBacklog {
		t.Errorf("child provenance = cause %q by %q class %q", child.Cause, child.SubmittedBy, child.BudgetClass)
	}
	if !strings.Contains(string(child.Snapshot), "Do: sub-task one") {
		t.Errorf("child snapshot did not materialize: %s", child.Snapshot)
	}
	if status, body = h.bridgeTool(claim.AttemptID, "forge_directive_run", map[string]any{"directive": "not-a-tool", "objective": "x"}); status != http.StatusBadRequest || !strings.Contains(string(body), "not tool-flagged") {
		t.Errorf("private directive = %d %s", status, body)
	}
	if status, _ = h.bridgeTool(claim.AttemptID, "forge_directive_run", map[string]any{"directive": "spawnable", "objective": "  "}); status != http.StatusBadRequest {
		t.Errorf("empty objective = %d", status)
	}
	if status, _ = h.bridgeTool(claim.AttemptID, "forge_directive_run", map[string]any{"directive": "spawnable", "objective": "x", "class": "interactive"}); status != http.StatusBadRequest {
		t.Errorf("interactive class = %d", status)
	}

	// Fan-out cap: 4 more spawns fill the 5-cap; the 6th refuses.
	for i := 0; i < 4; i++ {
		if status, body = h.bridgeTool(claim.AttemptID, "forge_directive_run", map[string]any{"directive": "spawnable", "objective": fmt.Sprintf("fill %d", i)}); status != http.StatusOK {
			t.Fatalf("fill %d = %d %s", i, status, body)
		}
	}
	if status, body = h.bridgeTool(claim.AttemptID, "forge_directive_run", map[string]any{"directive": "spawnable", "objective": "one too many"}); status != http.StatusBadRequest || !strings.Contains(string(body), "cap") {
		t.Errorf("6th spawn = %d %s", status, body)
	}

	// forge_workflow_run: fires the tool-flagged flow, refuses the private one.
	status, body = h.bridgeTool(claim.AttemptID, "forge_workflow_run", map[string]any{"workflow": "callable-flow", "objective": "go"})
	if status != http.StatusOK || !strings.Contains(string(body), "run_id") {
		t.Fatalf("workflow run = %d %s", status, body)
	}
	if status, body = h.bridgeTool(claim.AttemptID, "forge_workflow_run", map[string]any{"workflow": "private-flow"}); status != http.StatusBadRequest || !strings.Contains(string(body), "not tool-flagged") {
		t.Errorf("private workflow = %d %s", status, body)
	}
}

// Depth cap: a spawned work's own agent may not spawn further.
func TestToolBridgeDepthCap(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{
		"directives/spawnable.md": "---\nmode: run\nmodel: haiku\ntool: true\ndescription: d\n---\nDo: {{objective}}",
	})
	h.createRoutine("caller")
	h.run("caller")
	parent := h.mustClaim("depth-1")

	status, body := h.bridgeTool(parent.AttemptID, "forge_directive_run", map[string]any{"directive": "spawnable", "objective": "level one", "class": "normal"})
	if status != http.StatusOK {
		t.Fatalf("first spawn = %d %s", status, body)
	}
	// Finish the parent so the worker can claim the child.
	h.heartbeat(parent, model.Preparing, 0)
	h.heartbeat(parent, model.Running, 41)
	h.complete(parent, completeRequest(model.Succeeded, h.clock.Now()))

	childClaim := h.mustClaim("depth-2")
	childWork, err := h.st.GetWork(context.Background(), mustTargetWork(t, h, childClaim.TargetID))
	if err != nil || childWork.Cause != model.CauseTool {
		t.Fatalf("claimed the wrong work: %+v, %v", childWork, err)
	}
	status, body = h.bridgeTool(childClaim.AttemptID, "forge_directive_run", map[string]any{"directive": "spawnable", "objective": "level two"})
	if status != http.StatusBadRequest || !strings.Contains(string(body), "may not spawn further") {
		t.Fatalf("depth-2 spawn = %d %s", status, body)
	}
}

func mustTargetWork(t *testing.T, h *harness, targetID string) string {
	t.Helper()
	tg, err := h.st.GetTarget(context.Background(), targetID)
	if err != nil {
		t.Fatal(err)
	}
	return tg.WorkID
}
