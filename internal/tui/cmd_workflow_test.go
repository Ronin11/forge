package tui

import (
	"strings"
	"testing"

	"github.com/BurntSushi/toml"

	"forge/internal/core/store"
)

// The graph form must survive the TOML round-trip `workflow show` → `$EDITOR`
// → PUT: node configs are maps, so every field — including keys the CLI does
// not know about — comes back intact.
func TestWorkflowGraphTOMLRoundTrip(t *testing.T) {
	wf := store.Workflow{Name: "branchy", Graph: &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{
			{ID: "build", Type: store.NodeDirective, Config: map[string]any{"directive": "build-all", "objective": "build {{repo}}"}, Position: store.GraphPosition{X: 240, Y: 120}},
			{ID: "route", Type: store.NodeSwitch, Config: map[string]any{"expression": "input.steps.build.output.kind"}},
			{ID: "fix", Type: store.NodeDirective, Config: map[string]any{"directive": "fix"}},
		},
		Edges: []store.WorkflowGraphEdge{
			{From: "build", To: "route"},
			{From: "route", To: "fix", Default: true},
			{From: "fix", To: "build", Loop: true, MaxIterations: 3},
		},
	}}
	var b strings.Builder
	if err := toml.NewEncoder(&b).Encode(wf); err != nil {
		t.Fatalf("encode: %v", err)
	}
	var back store.Workflow
	if _, err := toml.Decode(b.String(), &back); err != nil {
		t.Fatalf("decode: %v\n%s", err, b.String())
	}
	if back.Graph == nil || len(back.Graph.Nodes) != 3 || len(back.Graph.Edges) != 3 {
		t.Fatalf("graph = %+v", back.Graph)
	}
	if cfg, err := back.Graph.Nodes[0].DirectiveConfig(); err != nil || cfg.Directive != "build-all" || cfg.Objective != "build {{repo}}" {
		t.Errorf("build config = %+v, %v", cfg, err)
	}
	if e := back.Graph.Edges[2]; !e.Loop || e.MaxIterations != 3 {
		t.Errorf("loop edge = %+v", e)
	}
	if p := back.Graph.Nodes[0].Position; p.X != 240 || p.Y != 120 {
		t.Errorf("position = %+v", p)
	}
}
