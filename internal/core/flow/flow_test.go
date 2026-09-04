package flow

import (
	"encoding/json"
	"fmt"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// sim drives Evaluate the way the real driver does: apply the diff to an
// in-memory row set, materialize Starts as fake Works, and let the test move
// Work states between rounds.
type sim struct {
	t       *testing.T
	graph   *store.WorkflowGraph
	nodes   []store.RunNode
	works   map[string]model.WorkState // work id → state
	scripts map[string]ScriptResult    // instance id → pending execution result
	seq     int
	runSt   string
}

func newSim(t *testing.T, g *store.WorkflowGraph) *sim {
	t.Helper()
	if err := g.Validate(); err != nil {
		t.Fatalf("test graph invalid: %v", err)
	}
	return &sim{t: t, graph: g, works: map[string]model.WorkState{}, scripts: map[string]ScriptResult{}, runSt: store.RunRunning}
}

// settle evaluates and applies until stable, materializing Starts as pending
// fake works. Scripts/switches are completed by the fn (nil = fail the test
// if one turns up).
func (s *sim) settle() {
	s.t.Helper()
	for range 50 {
		diff, err := Evaluate(Input{RunID: "run-1", Graph: s.graph, RunStatus: s.runSt, Nodes: s.nodes, WorkStates: s.works, ScriptResults: s.scripts})
		if err != nil {
			s.t.Fatal(err)
		}
		if diff.RunStatus != "" {
			s.runSt = diff.RunStatus
		}
		if diff.Empty() {
			return
		}
		for _, c := range diff.Creates {
			c.ID = fmt.Sprintf("inst-%s-%d", c.NodeID, c.Iteration)
			s.nodes = append(s.nodes, c)
		}
		for _, u := range diff.Updates {
			for i := range s.nodes {
				if s.nodes[i].ID == u.Node.ID {
					if s.nodes[i].Status != u.From {
						s.t.Fatalf("CAS mismatch on %s: row %s, expected %s", u.Node.ID, s.nodes[i].Status, u.From)
					}
					s.nodes[i] = u.Node
				}
			}
		}
		for _, st := range diff.Starts {
			s.seq++
			work := fmt.Sprintf("work-%d", s.seq)
			s.works[work] = model.WorkRunning
			for i := range s.nodes {
				if s.nodes[i].NodeID == st.NodeID && s.nodes[i].Iteration == st.Iteration {
					s.nodes[i].Status = store.NodeRunning
					s.nodes[i].WorkID = work
				}
			}
		}
	}
	s.t.Fatal("simulation did not settle")
}

// instance fetches a node's instance by id+iteration.
func (s *sim) instance(nodeID string, iter int) *store.RunNode {
	for i := range s.nodes {
		if s.nodes[i].NodeID == nodeID && s.nodes[i].Iteration == iter {
			return &s.nodes[i]
		}
	}
	return nil
}

// finishWork moves a node instance's work to a state and re-settles.
func (s *sim) finishWork(nodeID string, iter int, st model.WorkState) {
	s.t.Helper()
	inst := s.instance(nodeID, iter)
	if inst == nil || inst.WorkID == "" {
		s.t.Fatalf("%s#%d has no work (instance %+v)", nodeID, iter, inst)
	}
	s.works[inst.WorkID] = st
	s.settle()
}

// completeScript feeds a ready script/switch instance's execution result the
// way the driver does, then re-settles.
func (s *sim) completeScript(nodeID string, iter int, status string, output string) {
	s.t.Helper()
	inst := s.instance(nodeID, iter)
	if inst == nil || inst.Status != store.NodeReady {
		s.t.Fatalf("%s#%d not ready: %+v", nodeID, iter, inst)
	}
	s.scripts[inst.ID] = ScriptResult{Status: status, Output: json.RawMessage(output)}
	s.settle()
}

func (s *sim) wantStatus(nodeID string, iter int, status string) {
	s.t.Helper()
	inst := s.instance(nodeID, iter)
	if inst == nil {
		s.t.Fatalf("%s#%d: no instance", nodeID, iter)
	}
	if inst.Status != status {
		s.t.Errorf("%s#%d = %s, want %s", nodeID, iter, inst.Status, status)
	}
}

func (s *sim) wantNoInstance(nodeID string, iter int) {
	s.t.Helper()
	if inst := s.instance(nodeID, iter); inst != nil {
		s.t.Errorf("%s#%d exists: %+v", nodeID, iter, inst)
	}
}

func directiveNode(id string) store.WorkflowNode {
	return store.WorkflowNode{ID: id, Type: store.NodeDirective, Config: map[string]any{"directive": "r-" + id}}
}

func TestChainProgression(t *testing.T) {
	s := newSim(t, &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{directiveNode("a"), directiveNode("b")},
		Edges: []store.WorkflowGraphEdge{{From: "a", To: "b"}},
	})
	s.settle()
	s.wantStatus("a", 1, store.NodeRunning) // root materialized
	s.wantNoInstance("b", 1)                // dependants are never pre-created
	s.finishWork("a", 1, model.WorkSucceeded)
	s.wantStatus("a", 1, store.NodeSucceeded)
	s.wantStatus("b", 1, store.NodeRunning)
	if s.instance("b", 1).Edges["a|"] != "taken" {
		t.Errorf("b edges = %+v", s.instance("b", 1).Edges)
	}
	s.finishWork("b", 1, model.WorkMerged)
	if s.runSt != store.RunSucceeded {
		t.Errorf("run = %s, want succeeded", s.runSt)
	}
}

func TestFailureWithoutFailureEdgeSkipsAndFails(t *testing.T) {
	s := newSim(t, &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{directiveNode("a"), directiveNode("b"), directiveNode("c")},
		Edges: []store.WorkflowGraphEdge{{From: "a", To: "b"}, {From: "b", To: "c"}},
	})
	s.settle()
	s.finishWork("a", 1, model.WorkFailed)
	s.wantStatus("b", 1, store.NodeSkipped) // the cascade, not a permanent wedge
	s.wantStatus("c", 1, store.NodeSkipped)
	if s.runSt != store.RunFailed {
		t.Errorf("run = %s, want failed", s.runSt)
	}
}

func TestFailureEdgeRoutes(t *testing.T) {
	s := newSim(t, &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{directiveNode("a"), directiveNode("ok"), directiveNode("cleanup")},
		Edges: []store.WorkflowGraphEdge{
			{From: "a", To: "ok", When: store.WhenSuccess},
			{From: "a", To: "cleanup", When: store.WhenFailure},
		},
	})
	s.settle()
	s.finishWork("a", 1, model.WorkFailed)
	s.wantStatus("ok", 1, store.NodeSkipped)
	s.wantStatus("cleanup", 1, store.NodeRunning)
	s.finishWork("cleanup", 1, model.WorkSucceeded)
	// The failing node's latest instance failed, so the run reports failed
	// even though routing handled it.
	if s.runSt != store.RunFailed {
		t.Errorf("run = %s, want failed", s.runSt)
	}
}

func TestParallelJoinAll(t *testing.T) {
	s := newSim(t, &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{directiveNode("fan"), directiveNode("b"), directiveNode("c"), {ID: "j", Type: store.NodeJoin}, directiveNode("after")},
		Edges: []store.WorkflowGraphEdge{
			{From: "fan", To: "b"}, {From: "fan", To: "c"},
			{From: "b", To: "j"}, {From: "c", To: "j"},
			{From: "j", To: "after"},
		},
	})
	s.settle()
	s.finishWork("fan", 1, model.WorkSucceeded)
	s.wantStatus("b", 1, store.NodeRunning)
	s.wantStatus("c", 1, store.NodeRunning)
	s.finishWork("b", 1, model.WorkSucceeded)
	s.wantStatus("j", 1, store.NodePending) // all-mode: waits for c
	s.wantNoInstance("after", 1)
	s.finishWork("c", 1, model.WorkSucceeded)
	s.wantStatus("j", 1, store.NodeSucceeded)
	s.wantStatus("after", 1, store.NodeRunning)
}

func TestJoinAnyFiresOnFirst(t *testing.T) {
	s := newSim(t, &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{directiveNode("fan"), directiveNode("b"), directiveNode("c"), {ID: "j", Type: store.NodeJoin, Config: map[string]any{"mode": "any"}}, directiveNode("after")},
		Edges: []store.WorkflowGraphEdge{
			{From: "fan", To: "b"}, {From: "fan", To: "c"},
			{From: "b", To: "j"}, {From: "c", To: "j"},
			{From: "j", To: "after"},
		},
	})
	s.settle()
	s.finishWork("fan", 1, model.WorkSucceeded)
	s.finishWork("b", 1, model.WorkSucceeded)
	s.wantStatus("j", 1, store.NodeSucceeded) // any-mode: first token wins
	s.wantStatus("after", 1, store.NodeRunning)
	// The loser keeps running; finishing it does not re-fire the join.
	s.finishWork("c", 1, model.WorkSucceeded)
	s.wantStatus("j", 1, store.NodeSucceeded)
	s.wantNoInstance("j", 2)
	s.wantNoInstance("after", 2)
}

func TestJoinAllToleratesDeadBranch(t *testing.T) {
	s := newSim(t, &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{directiveNode("a"), directiveNode("good"), directiveNode("bad"), {ID: "j", Type: store.NodeJoin}},
		Edges: []store.WorkflowGraphEdge{
			{From: "a", To: "good", When: store.WhenSuccess}, {From: "a", To: "bad", When: store.WhenFailure},
			{From: "good", To: "j"}, {From: "bad", To: "j"},
		},
	})
	s.settle()
	s.finishWork("a", 1, model.WorkSucceeded)
	s.wantStatus("bad", 1, store.NodeSkipped)
	s.finishWork("good", 1, model.WorkSucceeded)
	// One branch dead, one taken: the join fires rather than waiting forever.
	s.wantStatus("j", 1, store.NodeSucceeded)
	if s.runSt != store.RunSucceeded {
		t.Errorf("run = %s, want succeeded", s.runSt)
	}
}

func TestLoopRetriesUntilCap(t *testing.T) {
	s := newSim(t, &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{directiveNode("build"), directiveNode("fix")},
		Edges: []store.WorkflowGraphEdge{
			{From: "build", To: "fix", When: store.WhenFailure},
			{From: "fix", To: "build", Loop: true, MaxIterations: 2},
		},
	})
	s.settle()
	s.finishWork("build", 1, model.WorkFailed)
	s.wantStatus("fix", 1, store.NodeRunning)
	s.finishWork("fix", 1, model.WorkSucceeded)
	s.wantStatus("build", 2, store.NodeRunning) // loop re-entry, iteration 2
	s.finishWork("build", 2, model.WorkFailed)
	s.finishWork("fix", 2, model.WorkSucceeded)
	s.wantStatus("build", 3, store.NodeRunning)
	s.finishWork("build", 3, model.WorkFailed)
	s.finishWork("fix", 3, model.WorkSucceeded)
	// Cap = 2 firings: three build instances total, no fourth.
	s.wantNoInstance("build", 4)
	if s.runSt != store.RunFailed {
		t.Errorf("run = %s, want failed (build's latest failed)", s.runSt)
	}
}

func TestLoopRetrySucceeds(t *testing.T) {
	s := newSim(t, &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{directiveNode("build"), directiveNode("fix"), directiveNode("ship")},
		Edges: []store.WorkflowGraphEdge{
			{From: "build", To: "ship", When: store.WhenSuccess},
			{From: "build", To: "fix", When: store.WhenFailure},
			{From: "fix", To: "build", Loop: true, MaxIterations: 3},
		},
	})
	s.settle()
	s.finishWork("build", 1, model.WorkFailed)
	s.finishWork("fix", 1, model.WorkSucceeded)
	s.finishWork("build", 2, model.WorkSucceeded)
	s.wantStatus("ship", 2, store.NodeRunning) // downstream re-fires at the new wave
	s.finishWork("ship", 2, model.WorkSucceeded)
	if s.runSt != store.RunSucceeded {
		t.Errorf("run = %s, want succeeded (latest build succeeded)", s.runSt)
	}
}

func TestSwitchRoutesCaseAndDefault(t *testing.T) {
	g := &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{
			directiveNode("a"),
			{ID: "route", Type: store.NodeSwitch, Config: map[string]any{"expression": "input.x"}},
			directiveNode("docs"), directiveNode("other"),
		},
		Edges: []store.WorkflowGraphEdge{
			{From: "a", To: "route"},
			{From: "route", To: "docs", When: store.WhenCase, Case: "docs"},
			{From: "route", To: "other", Default: true},
		},
	}
	s := newSim(t, g)
	s.settle()
	s.finishWork("a", 1, model.WorkSucceeded)
	s.wantStatus("route", 1, store.NodeReady) // switches wait for the driver
	s.completeScript("route", 1, store.NodeSucceeded, `{"case":"docs"}`)
	s.wantStatus("docs", 1, store.NodeRunning)
	s.wantStatus("other", 1, store.NodeSkipped)

	// Same graph, unmatched case → the default arm.
	s2 := newSim(t, g)
	s2.settle()
	s2.finishWork("a", 1, model.WorkSucceeded)
	s2.completeScript("route", 1, store.NodeSucceeded, `{"case":"perf"}`)
	s2.wantStatus("docs", 1, store.NodeSkipped)
	s2.wantStatus("other", 1, store.NodeRunning)
}

func TestSwitchFailureRoutesFailureEdge(t *testing.T) {
	s := newSim(t, &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{
			directiveNode("a"),
			{ID: "route", Type: store.NodeSwitch, Config: map[string]any{"expression": "input.x"}},
			directiveNode("docs"), directiveNode("rescue"),
		},
		Edges: []store.WorkflowGraphEdge{
			{From: "a", To: "route"},
			{From: "route", To: "docs", When: store.WhenCase, Case: "docs"},
			{From: "route", To: "rescue", When: store.WhenFailure},
		},
	})
	s.settle()
	s.finishWork("a", 1, model.WorkSucceeded)
	s.completeScript("route", 1, store.NodeFailed, "")
	s.wantStatus("docs", 1, store.NodeSkipped)
	s.wantStatus("rescue", 1, store.NodeRunning)
}

func TestCancelledWorkCancelsRun(t *testing.T) {
	s := newSim(t, &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{directiveNode("a"), directiveNode("b")},
		Edges: []store.WorkflowGraphEdge{{From: "a", To: "b"}},
	})
	s.settle()
	s.finishWork("a", 1, model.WorkCancelled)
	s.wantStatus("b", 1, store.NodeSkipped)
	if s.runSt != store.RunCancelled {
		t.Errorf("run = %s, want cancelled", s.runSt)
	}
}

func TestWaitingHumanKeepsNodeRunning(t *testing.T) {
	s := newSim(t, &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{directiveNode("a"), directiveNode("b")},
		Edges: []store.WorkflowGraphEdge{{From: "a", To: "b"}},
	})
	s.settle()
	s.finishWork("a", 1, model.WorkWaitingHuman) // not terminal
	s.wantStatus("a", 1, store.NodeRunning)
	s.wantNoInstance("b", 1)
	if s.runSt != store.RunRunning {
		t.Errorf("run = %s, want still running", s.runSt)
	}
}

// A second Evaluate over an unchanged row set must be a no-op: the engine's
// idempotence rests on it.
func TestEvaluateIsStable(t *testing.T) {
	s := newSim(t, &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{directiveNode("a"), directiveNode("b")},
		Edges: []store.WorkflowGraphEdge{{From: "a", To: "b"}},
	})
	s.settle()
	s.finishWork("a", 1, model.WorkSucceeded)
	diff, err := Evaluate(Input{RunID: "run-1", Graph: s.graph, Nodes: s.nodes, WorkStates: s.works})
	if err != nil {
		t.Fatal(err)
	}
	if !diff.Empty() {
		t.Errorf("stable state produced a diff: %+v", diff)
	}
}
