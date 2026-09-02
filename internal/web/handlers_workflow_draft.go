package web

// The workflow drafter: the editor's "describe it, get a graph" seam. The
// operator talks through what they want; one model call (the concierge's
// daemon-injected primitive) turns the description — plus the current graph,
// when editing — into a WorkflowGraph the editor loads for review. Nothing is
// saved here: the human refines and saves, so the model's output goes through
// exactly the same validation and approval path as a hand-drawn graph. An
// invalid graph gets one repair round with the validator's message before the
// error surfaces.

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"strings"

	"forge/internal/core/flow"
	"forge/internal/core/store"
)

const draftModel = "sonnet"

// draftRequest is POST /api/v1/workflows/draft.
type draftRequest struct {
	Description string               `json:"description"`
	Graph       *store.WorkflowGraph `json:"graph,omitempty"` // the editor's current graph, when editing
}

// draftResponse carries the drafted graph and the model's short explanation.
type draftResponse struct {
	Graph *store.WorkflowGraph `json:"graph"`
	Notes string               `json:"notes,omitempty"`
}

func (s *Server) draftWorkflow(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.modelCall == nil {
		return 0, nil, badRequest("the workflow drafter needs the daemon's model access, which this process does not have")
	}
	var req draftRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	req.Description = strings.TrimSpace(req.Description)
	if req.Description == "" {
		return 0, nil, badRequest("describe the workflow you want")
	}
	system, err := s.draftSystemPrompt(ctx)
	if err != nil {
		return 0, nil, err
	}
	user := draftUserPrompt(req)
	graph, notes, verr := s.draftOnce(ctx, system, user)
	if verr != nil && graph != nil {
		// One repair round: hand the validator's complaint back.
		graph, notes, verr = s.draftOnce(ctx, system, user+"\n\nYour previous attempt was rejected: "+verr.Error()+"\nFix it and return the corrected JSON.")
	}
	if verr != nil {
		return 0, nil, badRequest("the drafted graph did not validate: %v — rephrase and try again", verr)
	}
	s.log.InfoContext(ctx, "workflow drafted", "nodes", len(graph.Nodes), "edges", len(graph.Edges))
	return http.StatusOK, draftResponse{Graph: graph, Notes: notes}, nil
}

// draftOnce is one model call plus validation. A nil graph with an error
// means the output was unusable (no retry material); a non-nil graph with an
// error is retryable.
func (s *Server) draftOnce(ctx context.Context, system, user string) (*store.WorkflowGraph, string, error) {
	raw, err := s.modelCall(ctx, system, user, draftModel)
	if err != nil {
		return nil, "", fmt.Errorf("model call: %w", err)
	}
	var out struct {
		Graph *store.WorkflowGraph `json:"graph"`
		Notes string               `json:"notes"`
	}
	body := extractJSON(raw)
	if err := json.Unmarshal([]byte(body), &out); err != nil || out.Graph == nil {
		return nil, "", fmt.Errorf("the model did not return a graph")
	}
	if err := out.Graph.Validate(); err != nil {
		return out.Graph, out.Notes, err
	}
	for _, n := range out.Graph.Nodes {
		switch n.Type {
		case store.NodeScript:
			if cfg, err := n.ScriptConfig(); err == nil {
				if err := flow.CompileScript(cfg.Source); err != nil {
					return out.Graph, out.Notes, fmt.Errorf("node %s: %v", n.ID, err)
				}
			}
		case store.NodeSwitch:
			if cfg, err := n.SwitchConfig(); err == nil {
				if err := flow.CompileSwitch(cfg.Expression); err != nil {
					return out.Graph, out.Notes, fmt.Errorf("node %s: %v", n.ID, err)
				}
			}
		}
	}
	return out.Graph, out.Notes, nil
}

// extractJSON tolerates a model wrapping its JSON in prose or a code fence.
func extractJSON(raw string) string {
	raw = strings.TrimSpace(raw)
	if i := strings.Index(raw, "{"); i >= 0 {
		if j := strings.LastIndex(raw, "}"); j > i {
			return raw[i : j+1]
		}
	}
	return raw
}

// draftSystemPrompt teaches the graph schema and lists what exists to build
// with: the registered routines and repositories.
func (s *Server) draftSystemPrompt(ctx context.Context) (string, error) {
	routines, err := s.store.ListRoutines(ctx, false)
	if err != nil {
		return "", err
	}
	repos, err := s.store.Repositories(ctx)
	if err != nil {
		return "", err
	}
	var b strings.Builder
	b.WriteString(`You design workflow graphs for Forge, a system that runs coding agents against local git repositories. You return ONLY a JSON object, no prose outside it:
{"graph": {"nodes": [...], "edges": [...]}, "notes": "<2-3 sentences for the human: what the graph does and any assumptions>"}

A node is {"id": "<lower-case-slug>", "type": "routine"|"script"|"switch"|"join", "config": {...}}.
- routine: an agent runs a saved routine. config: {"routine": "<existing routine name>", "objective": "<optional instructions for this run; may embed {{steps.<node>.output.<path>}} or {{run.objective}}>", "repositories": ["<optional override>"]}
- script: JavaScript in a sandbox (no filesystem/network). config: {"source": "function main(input) { ... return <json>; }"}. input.steps.<node> = {status, state, summary, output}; the return value becomes the node's output.
- switch: routes on an expression. config: {"expression": "<JS expression over input, e.g. input.steps.triage.output.kind>"}. Its String() value picks the matching case edge.
- join: fan-in. config: {"mode": "all"|"any"}. Needs >= 2 incoming edges.

An edge is {"from": "<id>", "to": "<id>", "when": "success"|"failure"|"always"|"case", "case": "<value, case edges only>", "default": true (a switch's else-arm), "stack_on": true (start on the upstream unmerged branch), "loop": true, "max_iterations": 1-20}.
- Omitting "when" means success. A switch's outgoing edges must be case/default (plus optionally one failure edge).
- The graph without loop edges must be acyclic with at least one root; every intentional cycle is one edge marked "loop": true with "max_iterations".
- A failed node with no failure edge skips everything downstream and fails the run — add failure/always edges deliberately.
- Prefer few, clear nodes. Use scripts only for real logic (shaping outputs, thresholds), not as glue for its own sake.

Existing routines (use these names; do not invent routines):
`)
	for _, rt := range routines {
		prompt := rt.Prompt
		if len(prompt) > 140 {
			prompt = prompt[:140] + "…"
		}
		fmt.Fprintf(&b, "- %s (mode %s, repos %s): %s\n", rt.Name, rt.Mode, strings.Join(rt.Repositories, ","), strings.ReplaceAll(prompt, "\n", " "))
	}
	b.WriteString("\nRegistered repositories: ")
	names := make([]string, len(repos))
	for i, rep := range repos {
		names[i] = rep.Name
	}
	b.WriteString(strings.Join(names, ", "))
	b.WriteString("\nIf the description needs a routine that does not exist, pick the closest existing routine and say so in notes — never invent a routine name.")
	return b.String(), nil
}

func draftUserPrompt(req draftRequest) string {
	var b strings.Builder
	if req.Graph != nil && len(req.Graph.Nodes) > 0 {
		current, err := json.Marshal(req.Graph)
		if err == nil {
			b.WriteString("Current graph (edit it — keep node ids and positions where they still apply):\n")
			b.Write(current)
			b.WriteString("\n\n")
		}
	}
	b.WriteString("Request: ")
	b.WriteString(req.Description)
	return b.String()
}
