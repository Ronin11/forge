package store

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
	"time"
)

func createTestWorkflow(t *testing.T, s *Store, wf *Workflow) {
	t.Helper()
	if err := s.Write(context.Background(), func(tx *Tx) error { return tx.CreateWorkflow(context.Background(), wf) }); err != nil {
		t.Fatal(err)
	}
}

func TestWorkflowCreateGraphForm(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	wf := &Workflow{Name: "branchy", Graph: &WorkflowGraph{
		Nodes: []WorkflowNode{
			{ID: "build", Type: NodeDirective, Config: map[string]any{"directive": "build-all", "keep": "me"}},
			{ID: "triage", Type: NodeScript, Config: map[string]any{"source": "function main(input) { return {ok: true} }"}},
			{ID: "route", Type: NodeSwitch, Config: map[string]any{"expression": "input.steps.build.output.kind"}},
			{ID: "docs", Type: NodeDirective, Config: map[string]any{"directive": "docs"}},
			{ID: "fix", Type: NodeDirective, Config: map[string]any{"directive": "fix"}},
		},
		Edges: []WorkflowGraphEdge{
			{From: "build", To: "triage", When: WhenFailure},
			{From: "build", To: "route", When: WhenSuccess},
			{From: "route", To: "docs", When: WhenCase, Case: "docs"},
			{From: "route", To: "fix", Default: true},
			{From: "fix", To: "build", Loop: true, MaxIterations: 3},
		},
	}}
	createTestWorkflow(t, s, wf)
	got, err := s.GetWorkflow(ctx, "branchy")
	if err != nil {
		t.Fatal(err)
	}
	if got.Generation != 1 || got.Graph == nil {
		t.Fatalf("workflow = %+v", got)
	}
	if len(got.Graph.Nodes) != 5 || len(got.Graph.Edges) != 5 {
		t.Fatalf("graph = %+v", got.Graph)
	}
	// Unknown config keys survive the round-trip: the editor's contract.
	if got.Graph.Node("build").Config["keep"] != "me" {
		t.Errorf("unknown config key lost: %+v", got.Graph.Node("build").Config)
	}
	if n := got.Graph.Node("build"); n.Type != NodeDirective {
		t.Fatalf("build node = %+v", n)
	} else if cfg, err := n.DirectiveConfig(); err != nil || cfg.Directive != "build-all" {
		t.Errorf("build config = %+v, %v", cfg, err)
	}
	// The generation snapshot exists and round-trips as a graph.
	var snap string
	if err := s.queryRow(ctx, `SELECT snapshot FROM workflow_generations WHERE workflow_id = ? AND generation = 1`, got.ID).Scan(&snap); err != nil {
		t.Fatal(err)
	}
	var decoded Workflow
	if err := json.Unmarshal([]byte(snap), &decoded); err != nil || decoded.Name != "branchy" || decoded.Graph == nil {
		t.Errorf("snapshot = %s, %v", snap, err)
	}
}

func TestWorkflowGraphValidate(t *testing.T) {
	directive := func(id string) WorkflowNode {
		return WorkflowNode{ID: id, Type: NodeDirective, Config: map[string]any{"directive": "r"}}
	}
	cases := []struct {
		name string
		g    WorkflowGraph
	}{
		{"no nodes", WorkflowGraph{}},
		{"bad node id", WorkflowGraph{Nodes: []WorkflowNode{{ID: "Bad Name", Type: NodeDirective, Config: map[string]any{"directive": "r"}}}}},
		{"unknown type", WorkflowGraph{Nodes: []WorkflowNode{{ID: "a", Type: "shell"}}}},
		{"duplicate node", WorkflowGraph{Nodes: []WorkflowNode{directive("a"), directive("a")}}},
		{"unknown edge target", WorkflowGraph{Nodes: []WorkflowNode{directive("a")}, Edges: []WorkflowGraphEdge{{From: "a", To: "ghost"}}}},
		{"undeclared cycle", WorkflowGraph{Nodes: []WorkflowNode{directive("a"), directive("b")}, Edges: []WorkflowGraphEdge{{From: "a", To: "b"}, {From: "b", To: "a"}}}},
		{"loop without cap", WorkflowGraph{Nodes: []WorkflowNode{directive("a"), directive("b")}, Edges: []WorkflowGraphEdge{{From: "a", To: "b"}, {From: "b", To: "a", Loop: true}}}},
		{"case off non-switch", WorkflowGraph{Nodes: []WorkflowNode{directive("a"), directive("b")}, Edges: []WorkflowGraphEdge{{From: "a", To: "b", When: WhenCase, Case: "x"}}}},
		{"switch without cases", WorkflowGraph{Nodes: []WorkflowNode{directive("a"), {ID: "sw", Type: NodeSwitch, Config: map[string]any{"expression": "1"}}}, Edges: []WorkflowGraphEdge{{From: "a", To: "sw"}}}},
		{"duplicate case", WorkflowGraph{
			Nodes: []WorkflowNode{{ID: "sw", Type: NodeSwitch, Config: map[string]any{"expression": "1"}}, directive("a"), directive("b")},
			Edges: []WorkflowGraphEdge{{From: "sw", To: "a", When: WhenCase, Case: "x"}, {From: "sw", To: "b", When: WhenCase, Case: "x"}},
		}},
		{"one-armed join", WorkflowGraph{
			Nodes: []WorkflowNode{directive("a"), {ID: "j", Type: NodeJoin}},
			Edges: []WorkflowGraphEdge{{From: "a", To: "j"}},
		}},
		{"empty script", WorkflowGraph{Nodes: []WorkflowNode{{ID: "s", Type: NodeScript, Config: map[string]any{}}}}},
		{"stack_on failure edge", WorkflowGraph{Nodes: []WorkflowNode{directive("a"), directive("b")}, Edges: []WorkflowGraphEdge{{From: "a", To: "b", When: WhenFailure, StackOn: true}}}},
	}
	for _, tc := range cases {
		if err := tc.g.Validate(); err == nil {
			t.Errorf("%s: validated", tc.name)
		}
	}
	ok := WorkflowGraph{
		Nodes: []WorkflowNode{directive("a"), directive("b"), directive("c"), {ID: "j", Type: NodeJoin, Config: map[string]any{"mode": "any"}}},
		Edges: []WorkflowGraphEdge{
			{From: "a", To: "b"}, {From: "a", To: "c", When: WhenAlways},
			{From: "b", To: "j"}, {From: "c", To: "j"},
			{From: "j", To: "a", Loop: true, MaxIterations: 2},
		},
	}
	if err := ok.Validate(); err != nil {
		t.Errorf("valid graph rejected: %v", err)
	}
}

func testGraph(directive string) *WorkflowGraph {
	return &WorkflowGraph{Nodes: []WorkflowNode{{ID: "a", Type: NodeDirective, Config: map[string]any{"directive": directive}}}}
}

func TestWorkflowUpdateGenerations(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	wf := &Workflow{Name: "wf", Graph: testGraph("r-a")}
	createTestWorkflow(t, s, wf)
	dup := &Workflow{Name: "wf", Graph: testGraph("r-a")}
	if err := s.Write(ctx, func(tx *Tx) error { return tx.CreateWorkflow(ctx, dup) }); !errors.Is(err, ErrConflict) {
		t.Fatalf("duplicate create = %v, want conflict", err)
	}
	wf.Graph.Nodes = append(wf.Graph.Nodes, WorkflowNode{ID: "b", Type: NodeDirective, Config: map[string]any{"directive": "r-b"}})
	wf.Graph.Edges = append(wf.Graph.Edges, WorkflowGraphEdge{From: "a", To: "b"})
	if err := s.Write(ctx, func(tx *Tx) error { return tx.UpdateWorkflow(ctx, wf, 1) }); err != nil {
		t.Fatal(err)
	}
	if wf.Generation != 2 {
		t.Fatalf("generation = %d, want 2", wf.Generation)
	}
	if err := s.Write(ctx, func(tx *Tx) error { return tx.UpdateWorkflow(ctx, wf, 1) }); !errors.Is(err, ErrStaleGeneration) {
		t.Fatalf("stale update = %v", err)
	}
	if err := s.Write(ctx, func(tx *Tx) error { return tx.UpdateWorkflow(ctx, &Workflow{Name: "ghost", Graph: wf.Graph}, 1) }); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing update = %v", err)
	}
	var n int
	if err := s.queryRow(ctx, `SELECT count(*) FROM workflow_generations WHERE workflow_id = ?`, wf.ID).Scan(&n); err != nil || n != 2 {
		t.Errorf("generation records = %d, %v", n, err)
	}
}

func TestWorkflowArchiveAndDue(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	wf := &Workflow{Name: "cron", Graph: testGraph("r"), Schedule: "0 3 * * *", ScheduleEnabled: true}
	createTestWorkflow(t, s, wf)
	due := time.Date(2026, 8, 30, 3, 0, 0, 0, time.UTC)
	if err := s.Write(ctx, func(tx *Tx) error { return tx.SetWorkflowNextDue(ctx, "cron", due) }); err != nil {
		t.Fatal(err)
	}
	got, err := s.DueWorkflows(ctx, due.Add(time.Minute))
	if err != nil || len(got) != 1 || got[0].Name != "cron" {
		t.Fatalf("due = %+v, %v", got, err)
	}
	if err := s.Write(ctx, func(tx *Tx) error { return tx.ArchiveWorkflow(ctx, "cron") }); err != nil {
		t.Fatal(err)
	}
	if got, err = s.DueWorkflows(ctx, due.Add(time.Minute)); err != nil || len(got) != 0 {
		t.Fatalf("archived workflow still due: %+v, %v", got, err)
	}
	// Archived: hidden from the default list, visible when asked, second archive 404s.
	if ws, err := s.ListWorkflows(ctx, false); err != nil || len(ws) != 0 {
		t.Fatalf("list = %+v, %v", ws, err)
	}
	if ws, err := s.ListWorkflows(ctx, true); err != nil || len(ws) != 1 || ws[0].ArchivedAt.IsZero() {
		t.Fatalf("list archived = %+v, %v", ws, err)
	}
	if err := s.Write(ctx, func(tx *Tx) error { return tx.ArchiveWorkflow(ctx, "cron") }); !errors.Is(err, ErrNotFound) {
		t.Fatalf("second archive = %v", err)
	}
}

// Description and Tool round-trip through create/update/scan and ride the
// generation snapshot.
func TestWorkflowMetadataRoundTrip(t *testing.T) {
	s := openTest(t)
	bg := context.Background()
	w := &Workflow{Name: "meta", Description: "does a thing", Tool: true, Graph: &WorkflowGraph{
		Nodes: []WorkflowNode{{ID: "only", Type: NodeDirective, Config: map[string]any{"directive": "triage-repo"}}},
	}}
	if err := s.Write(bg, func(tx *Tx) error { return tx.CreateWorkflow(bg, w) }); err != nil {
		t.Fatal(err)
	}
	got, err := s.GetWorkflow(bg, "meta")
	if err != nil || got.Description != "does a thing" || !got.Tool {
		t.Fatalf("round trip = %+v, %v", got, err)
	}
	got.Description, got.Tool = "does it better", false
	if err := s.Write(bg, func(tx *Tx) error { return tx.UpdateWorkflow(bg, got, 1) }); err != nil {
		t.Fatal(err)
	}
	again, err := s.GetWorkflow(bg, "meta")
	if err != nil || again.Description != "does it better" || again.Tool || again.Generation != 2 {
		t.Fatalf("after update = %+v, %v", again, err)
	}
}
