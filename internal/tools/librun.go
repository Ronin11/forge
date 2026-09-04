package tools

// The callable side of the tool/skill bridge: run a tool-flagged library
// script synchronously in the sandbox, spawn a sub-Work from a tool-flagged
// directive, or fire a tool-flagged workflow. The spawn/run closures live in
// the daemon (Deps.SpawnWork / Deps.StartWorkflowRun) — they own the
// guardrails and provenance; the tools own input validation and the
// tool-flag checks, so a clear refusal never costs a transaction.

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"forge/internal/core/flow"
	"forge/internal/core/model"
)

// ---- forge_script_run ----------------------------------------------------

type scriptRunTool struct{}

func (scriptRunTool) Name() string { return "forge_script_run" }
func (scriptRunTool) Description() string {
	return "Run a tool-flagged library script synchronously. input becomes input.params. .js scripts run in a sandbox (no filesystem or network); other languages run as daemon-side processes. Discover scripts and their input schemas with forge_library."
}
func (scriptRunTool) Where() string { return WhereDaemon }
func (scriptRunTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{
		"script":{"type":"string","description":"the library script name"},
		"input":{"type":"object","description":"parameters per the script's input schema"}
	},"required":["script"],"additionalProperties":false}`)
}

func (scriptRunTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	if req.Deps.Library == nil {
		return nil, fmt.Errorf("the library is unavailable in this process")
	}
	var in struct {
		Script string          `json:"script"`
		Input  json.RawMessage `json:"input"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	lib := req.Deps.Library()
	if lib == nil {
		return nil, fmt.Errorf("no library is loaded")
	}
	f := lib.Script(in.Script)
	if f == nil {
		return nil, BadInput("script %q is not in the library — search with forge_library", in.Script)
	}
	if !f.Tool {
		return nil, BadInput("script %q is not tool-flagged (add `tool: true` to its /**forge header)", in.Script)
	}
	input := flow.ScriptInput{}
	if len(in.Input) > 0 {
		var params any
		if err := json.Unmarshal(in.Input, &params); err != nil {
			return nil, BadInput("input: %v", err)
		}
		input.Params = params
	}
	out, err := flow.RunAny(f.Interpreter, f.Path, f.Body, input, f.TimeoutMS)
	if err != nil {
		return nil, BadInput("script %s: %v", in.Script, err)
	}
	return respond(map[string]any{"schema_version": SchemaVersion, "output": out})
}

// ---- forge_directive_run -------------------------------------------------

type directiveRunTool struct{}

func (directiveRunTool) Name() string { return "forge_directive_run" }
func (directiveRunTool) Description() string {
	return "Spawn a sub-task running a tool-flagged directive with your objective (async: returns the work id; the queue runs it). Guardrails: spawned work may not spawn further, at most 5 spawns per task, backlog class by default. Discover directives with forge_library."
}
func (directiveRunTool) Where() string { return WhereDaemon }
func (directiveRunTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{
		"directive":{"type":"string","description":"the library directive name"},
		"objective":{"type":"string","description":"what the spawned task should accomplish"},
		"repositories":{"type":"array","items":{"type":"string"},"description":"defaults to your repository"},
		"class":{"type":"string","enum":["backlog","normal"],"description":"budget class; default backlog"}
	},"required":["directive","objective"],"additionalProperties":false}`)
}

func (directiveRunTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	if req.Deps.SpawnWork == nil || req.Deps.Library == nil {
		return nil, fmt.Errorf("work spawning is unavailable in this process")
	}
	var in struct {
		Directive    string   `json:"directive"`
		Objective    string   `json:"objective"`
		Repositories []string `json:"repositories"`
		Class        string   `json:"class"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if strings.TrimSpace(in.Objective) == "" {
		return nil, BadInput("objective is required — say what the spawned task should accomplish")
	}
	lib := req.Deps.Library()
	if lib == nil {
		return nil, fmt.Errorf("no library is loaded")
	}
	d := lib.Directive(in.Directive)
	if d == nil {
		return nil, BadInput("directive %q is not in the library — search with forge_library", in.Directive)
	}
	if !d.Tool {
		return nil, BadInput("directive %q is not tool-flagged (add `tool: true` to its frontmatter)", in.Directive)
	}
	class := model.ClassBacklog
	switch in.Class {
	case "", "backlog":
	case "normal":
		class = model.ClassNormal
	default:
		return nil, BadInput("class %q: want backlog or normal", in.Class)
	}
	repos := in.Repositories
	if len(repos) == 0 && req.Attempt.Repository != "" {
		repos = []string{req.Attempt.Repository}
	}
	workID, err := req.Deps.SpawnWork(ctx, req.Attempt, SpawnInput{
		Directive: in.Directive, Objective: in.Objective, Repositories: repos, Class: class,
	})
	if err != nil {
		return nil, err
	}
	return respond(map[string]any{"schema_version": SchemaVersion, "work_id": workID,
		"note": "spawned asynchronously; the queue runs it — check forge_attempt/forge_queue, do not wait for it"})
}

// ---- forge_workflow_run --------------------------------------------------

type workflowRunTool struct{}

func (workflowRunTool) Name() string { return "forge_workflow_run" }
func (workflowRunTool) Description() string {
	return "Fire a tool-flagged workflow (async: returns the run id; the engine runs the graph). Discover workflows with forge_library."
}
func (workflowRunTool) Where() string { return WhereDaemon }
func (workflowRunTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{
		"workflow":{"type":"string","description":"the workflow name"},
		"objective":{"type":"string","description":"the run's objective ({{run.objective}})"},
		"repositories":{"type":"array","items":{"type":"string"},"description":"defaults to your repository"}
	},"required":["workflow"],"additionalProperties":false}`)
}

func (workflowRunTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	if req.Deps.StartWorkflowRun == nil {
		return nil, fmt.Errorf("workflow runs are unavailable in this process")
	}
	var in struct {
		Workflow     string   `json:"workflow"`
		Objective    string   `json:"objective"`
		Repositories []string `json:"repositories"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	wf, err := req.Deps.Store.GetWorkflow(ctx, in.Workflow)
	if err != nil {
		return nil, BadInput("workflow %q: %v", in.Workflow, err)
	}
	if !wf.ArchivedAt.IsZero() {
		return nil, BadInput("workflow %q is archived", in.Workflow)
	}
	if !wf.Tool {
		return nil, BadInput("workflow %q is not tool-flagged (enable 'callable as a tool' in its editor)", in.Workflow)
	}
	repos := in.Repositories
	if len(repos) == 0 && req.Attempt.Repository != "" {
		repos = []string{req.Attempt.Repository}
	}
	runID, err := req.Deps.StartWorkflowRun(ctx, req.Attempt, in.Workflow, in.Objective, repos)
	if err != nil {
		return nil, err
	}
	return respond(map[string]any{"schema_version": SchemaVersion, "run_id": runID,
		"note": "fired asynchronously; the engine runs the graph — do not wait for it"})
}
