package tools

// forge_library: the skills reading path. An agent searches the library
// (directives, personas, fragments, scripts) and the workflow definitions,
// then fetches full content by name — a directive read this way is a skill
// to follow inline; a tool-flagged script/directive/workflow is additionally
// callable through forge_script_run / forge_directive_run /
// forge_workflow_run. One static tool: the registry freezes at daemon start
// and the per-attempt MCP list never changes, so discovery is dynamic at
// call time instead of tool-per-item.

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"forge/internal/core/directives"
)

type libraryTool struct{}

func (libraryTool) Name() string { return "forge_library" }
func (libraryTool) Description() string {
	return "Search Forge's library (directives, personas, fragments, scripts) and workflows, or fetch one item's full content by name+kind. A directive is a skill: read it and follow it. Items marked tool:true are callable via forge_script_run, forge_directive_run, or forge_workflow_run."
}
func (libraryTool) Where() string { return WhereDaemon }
func (libraryTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{
		"query":{"type":"string","description":"search terms; all must match name, description, or body"},
		"kind":{"type":"string","enum":["directive","persona","fragment","script","workflow"],"description":"restrict search, or (with name) select the item to fetch"},
		"name":{"type":"string","description":"fetch this item's full content (requires kind)"},
		"limit":{"type":"integer","minimum":1,"maximum":50}
	},"additionalProperties":false}`)
}

func (libraryTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	if req.Deps.Library == nil {
		return nil, fmt.Errorf("the library is unavailable in this process")
	}
	lib := req.Deps.Library()
	if lib == nil {
		return nil, fmt.Errorf("no library is loaded")
	}
	var in struct {
		Query string `json:"query"`
		Kind  string `json:"kind"`
		Name  string `json:"name"`
		Limit int    `json:"limit"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if in.Name != "" {
		return libraryGet(ctx, req, lib, in.Kind, in.Name)
	}
	limit := in.Limit
	if limit <= 0 || limit > 50 {
		limit = 20
	}
	kinds := map[string]bool{}
	if in.Kind != "" {
		kinds[in.Kind] = true
	}
	hits := lib.Search(in.Query, kinds, 0)
	if in.Kind == "" || in.Kind == "workflow" {
		terms := strings.Fields(strings.ToLower(in.Query))
		wfs, err := req.Deps.Store.ListWorkflows(ctx, false)
		if err != nil {
			return nil, err
		}
		for _, wf := range wfs {
			if score, ok := directives.ScoreTerms(terms, wf.Name, wf.Description, ""); ok {
				hits = append(hits, directives.SearchHit{Name: wf.Name, Kind: "workflow", Description: wf.Description, Tool: wf.Tool, Score: score})
			}
		}
		directives.SortHits(hits)
	}
	if len(hits) > limit {
		hits = hits[:limit]
	}
	return respond(map[string]any{"schema_version": SchemaVersion, "hits": hits})
}

// libraryGet returns one item's full content.
func libraryGet(ctx context.Context, req Request, lib *directives.Library, kind, name string) (json.RawMessage, error) {
	if kind == "workflow" {
		wf, err := req.Deps.Store.GetWorkflow(ctx, name)
		if err != nil {
			return nil, BadInput("workflow %q: %v", name, err)
		}
		graph, err := json.Marshal(wf.Graph)
		if err != nil {
			return nil, err
		}
		return respond(map[string]any{
			"schema_version": SchemaVersion,
			"name":           wf.Name, "kind": "workflow", "description": wf.Description, "tool": wf.Tool,
			"graph": json.RawMessage(graph),
		})
	}
	f := lib.Fragment(name)
	if f == nil {
		return nil, BadInput("%q is not in the library", name)
	}
	if kind != "" && f.Kind() != kind {
		return nil, BadInput("%q is a %s, not a %s", name, f.Kind(), kind)
	}
	out := map[string]any{
		"schema_version": SchemaVersion,
		"name":           f.Name, "kind": f.Kind(), "description": f.Description, "tool": f.Tool,
		"content": f.Body,
	}
	if f.Directive {
		out["mode"], out["persona"], out["model"], out["effort"] = f.Mode, f.PersonaRef, f.Model, f.Effort
	}
	if f.Script {
		out["input_schema"], out["timeout_ms"] = json.RawMessage(orEmptySchema(f.InputSchema)), f.TimeoutMS
	}
	return respond(out)
}

func orEmptySchema(s string) string {
	if s == "" {
		return `{}`
	}
	return s
}
