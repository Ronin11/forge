package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"path/filepath"
	"testing"
	"time"
)

func createTestWorkflow(t *testing.T, s *Store, wf *Workflow) {
	t.Helper()
	if err := s.Write(context.Background(), func(tx *Tx) error { return tx.CreateWorkflow(context.Background(), wf) }); err != nil {
		t.Fatal(err)
	}
}

func TestWorkflowCreateConvertsChainToGraph(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	wf := &Workflow{Name: "nightly", Steps: []WorkflowStep{
		{Name: "lint", Routine: "lint-all"},
		{Name: "fix", Routine: "fix-lint"},
		{Name: "verify", Routine: "verify-all", After: []WorkflowEdge{{Step: "lint"}, {Step: "fix", StackOn: true}}},
	}}
	createTestWorkflow(t, s, wf)
	got, err := s.GetWorkflow(ctx, "nightly")
	if err != nil {
		t.Fatal(err)
	}
	if got.Generation != 1 || got.Graph == nil || len(got.Steps) != 0 {
		t.Fatalf("workflow = %+v", got)
	}
	g := got.Graph
	if len(g.Nodes) != 3 || len(g.Edges) != 3 {
		t.Fatalf("graph = %+v", g)
	}
	// The legacy defaults are materialized as edges: a step without `after`
	// follows the previous one on success.
	if e := g.Edges[0]; e.From != "lint" || e.To != "fix" || e.When != WhenSuccess {
		t.Errorf("edge 0 = %+v, want lint→fix on success", e)
	}
	if e := g.Edges[2]; e.From != "fix" || e.To != "verify" || !e.StackOn {
		t.Errorf("edge 2 = %+v, want fix→verify stacked", e)
	}
	if n := g.Node("lint"); n == nil || n.Type != NodeRoutine {
		t.Fatalf("lint node = %+v", g.Node("lint"))
	} else if cfg, err := n.RoutineConfig(); err != nil || cfg.Routine != "lint-all" {
		t.Errorf("lint config = %+v, %v", cfg, err)
	}
	// Conversion lays the chain out left to right.
	if g.Node("lint").Position.X >= g.Node("verify").Position.X {
		t.Errorf("positions not layered: %+v vs %+v", g.Node("lint").Position, g.Node("verify").Position)
	}
	// The generation snapshot exists and round-trips as a graph.
	var snap string
	if err := s.queryRow(ctx, `SELECT snapshot FROM workflow_generations WHERE workflow_id = ? AND generation = 1`, got.ID).Scan(&snap); err != nil {
		t.Fatal(err)
	}
	var decoded Workflow
	if err := json.Unmarshal([]byte(snap), &decoded); err != nil || decoded.Name != "nightly" || decoded.Graph == nil {
		t.Errorf("snapshot = %s, %v", snap, err)
	}
}

func TestWorkflowCreateGraphForm(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	wf := &Workflow{Name: "branchy", Graph: &WorkflowGraph{
		Nodes: []WorkflowNode{
			{ID: "build", Type: NodeRoutine, Config: map[string]any{"routine": "build-all", "keep": "me"}},
			{ID: "triage", Type: NodeScript, Config: map[string]any{"source": "function main(input) { return {ok: true} }"}},
			{ID: "route", Type: NodeSwitch, Config: map[string]any{"expression": "input.steps.build.output.kind"}},
			{ID: "docs", Type: NodeRoutine, Config: map[string]any{"routine": "docs"}},
			{ID: "fix", Type: NodeRoutine, Config: map[string]any{"routine": "fix"}},
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
	if len(got.Graph.Nodes) != 5 || len(got.Graph.Edges) != 5 {
		t.Fatalf("graph = %+v", got.Graph)
	}
	// Unknown config keys survive the round-trip: the editor's contract.
	if got.Graph.Node("build").Config["keep"] != "me" {
		t.Errorf("unknown config key lost: %+v", got.Graph.Node("build").Config)
	}
}

func TestWorkflowGraphValidate(t *testing.T) {
	routine := func(id string) WorkflowNode {
		return WorkflowNode{ID: id, Type: NodeRoutine, Config: map[string]any{"routine": "r"}}
	}
	cases := []struct {
		name string
		g    WorkflowGraph
	}{
		{"no nodes", WorkflowGraph{}},
		{"bad node id", WorkflowGraph{Nodes: []WorkflowNode{{ID: "Bad Name", Type: NodeRoutine, Config: map[string]any{"routine": "r"}}}}},
		{"unknown type", WorkflowGraph{Nodes: []WorkflowNode{{ID: "a", Type: "shell"}}}},
		{"duplicate node", WorkflowGraph{Nodes: []WorkflowNode{routine("a"), routine("a")}}},
		{"unknown edge target", WorkflowGraph{Nodes: []WorkflowNode{routine("a")}, Edges: []WorkflowGraphEdge{{From: "a", To: "ghost"}}}},
		{"undeclared cycle", WorkflowGraph{Nodes: []WorkflowNode{routine("a"), routine("b")}, Edges: []WorkflowGraphEdge{{From: "a", To: "b"}, {From: "b", To: "a"}}}},
		{"loop without cap", WorkflowGraph{Nodes: []WorkflowNode{routine("a"), routine("b")}, Edges: []WorkflowGraphEdge{{From: "a", To: "b"}, {From: "b", To: "a", Loop: true}}}},
		{"case off non-switch", WorkflowGraph{Nodes: []WorkflowNode{routine("a"), routine("b")}, Edges: []WorkflowGraphEdge{{From: "a", To: "b", When: WhenCase, Case: "x"}}}},
		{"switch without cases", WorkflowGraph{Nodes: []WorkflowNode{routine("a"), {ID: "sw", Type: NodeSwitch, Config: map[string]any{"expression": "1"}}}, Edges: []WorkflowGraphEdge{{From: "a", To: "sw"}}}},
		{"duplicate case", WorkflowGraph{
			Nodes: []WorkflowNode{{ID: "sw", Type: NodeSwitch, Config: map[string]any{"expression": "1"}}, routine("a"), routine("b")},
			Edges: []WorkflowGraphEdge{{From: "sw", To: "a", When: WhenCase, Case: "x"}, {From: "sw", To: "b", When: WhenCase, Case: "x"}},
		}},
		{"one-armed join", WorkflowGraph{
			Nodes: []WorkflowNode{routine("a"), {ID: "j", Type: NodeJoin}},
			Edges: []WorkflowGraphEdge{{From: "a", To: "j"}},
		}},
		{"empty script", WorkflowGraph{Nodes: []WorkflowNode{{ID: "s", Type: NodeScript, Config: map[string]any{}}}}},
		{"stack_on failure edge", WorkflowGraph{Nodes: []WorkflowNode{routine("a"), routine("b")}, Edges: []WorkflowGraphEdge{{From: "a", To: "b", When: WhenFailure, StackOn: true}}}},
	}
	for _, tc := range cases {
		if err := tc.g.Validate(); err == nil {
			t.Errorf("%s: validated", tc.name)
		}
	}
	ok := WorkflowGraph{
		Nodes: []WorkflowNode{routine("a"), routine("b"), routine("c"), {ID: "j", Type: NodeJoin, Config: map[string]any{"mode": "any"}}},
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

func TestWorkflowValidateLegacySteps(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	cases := []struct {
		name string
		wf   Workflow
	}{
		{"no steps", Workflow{Name: "empty"}},
		{"bad step name", Workflow{Name: "bad", Steps: []WorkflowStep{{Name: "Bad Name", Routine: "r"}}}},
		{"bad routine name", Workflow{Name: "bad", Steps: []WorkflowStep{{Name: "a", Routine: "No!"}}}},
		{"duplicate step", Workflow{Name: "dup", Steps: []WorkflowStep{{Name: "a", Routine: "r"}, {Name: "a", Routine: "r"}}}},
		{"forward reference", Workflow{Name: "fwd", Steps: []WorkflowStep{{Name: "a", Routine: "r", After: []WorkflowEdge{{Step: "b"}}}, {Name: "b", Routine: "r"}}}},
		{"self reference", Workflow{Name: "self", Steps: []WorkflowStep{{Name: "a", Routine: "r", After: []WorkflowEdge{{Step: "a"}}}}}},
		{"bad on", Workflow{Name: "on", Steps: []WorkflowStep{{Name: "a", Routine: "r"}, {Name: "b", Routine: "r", After: []WorkflowEdge{{Step: "a", On: "sometimes"}}}}}},
	}
	for _, tc := range cases {
		wf := tc.wf
		err := s.Write(ctx, func(tx *Tx) error { return tx.CreateWorkflow(ctx, &wf) })
		if err == nil {
			t.Errorf("%s: created", tc.name)
		}
	}
}

func TestWorkflowUpdateGenerations(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	wf := &Workflow{Name: "wf", Steps: []WorkflowStep{{Name: "a", Routine: "r-a"}}}
	createTestWorkflow(t, s, wf)
	dup := &Workflow{Name: "wf", Steps: []WorkflowStep{{Name: "a", Routine: "r-a"}}}
	if err := s.Write(ctx, func(tx *Tx) error { return tx.CreateWorkflow(ctx, dup) }); !errors.Is(err, ErrConflict) {
		t.Fatalf("duplicate create = %v, want conflict", err)
	}
	wf.Graph.Nodes = append(wf.Graph.Nodes, WorkflowNode{ID: "b", Type: NodeRoutine, Config: map[string]any{"routine": "r-b"}})
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
	wf := &Workflow{Name: "cron", Steps: []WorkflowStep{{Name: "a", Routine: "r"}}, Schedule: "0 3 * * *", ScheduleEnabled: true}
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

// TestWorkflowGraphBackfill writes a legacy row directly (steps only, graph
// NULL) and proves reopening the store persists the conversion and journals it.
func TestWorkflowGraphBackfill(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "forge.sqlite3")
	s, err := Open(ctx, path, Options{})
	if err != nil {
		t.Fatal(err)
	}
	err = s.Write(ctx, func(tx *Tx) error {
		_, err := tx.Exec(ctx, `INSERT INTO workflows (id, name, steps, schedule_enabled, generation, created_at, updated_at) VALUES (?, 'legacy', ?, 0, 1, ?, ?)`,
			"0123456789abcdef0123456789abcdef", `[{"name":"a","routine":"r"},{"name":"b","routine":"r2","after":[{"step":"a","on":"success"}]}]`,
			formatTime(tx.Now()), formatTime(tx.Now()))
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	// The pre-backfill row is already served converted (scan-side conversion).
	got, err := s.GetWorkflow(ctx, "legacy")
	if err != nil || got.Graph == nil || len(got.Graph.Nodes) != 2 {
		t.Fatalf("pre-backfill read = %+v, %v", got, err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	s2, err := Open(ctx, path, Options{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := s2.Close(); err != nil {
			t.Error(err)
		}
	})
	var graph sql.NullString
	if err := s2.queryRow(ctx, `SELECT graph FROM workflows WHERE name = 'legacy'`).Scan(&graph); err != nil {
		t.Fatal(err)
	}
	if !graph.Valid || graph.String == "" {
		t.Fatal("backfill did not persist the graph")
	}
	var n int
	if err := s2.queryRow(ctx, `SELECT count(*) FROM journal WHERE kind = 'workflow.graph_migrated'`).Scan(&n); err != nil || n != 1 {
		t.Errorf("graph_migrated journal rows = %d, %v", n, err)
	}
}
