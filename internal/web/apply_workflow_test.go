package web

import (
	"strings"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// A workflow-kind proposal creates a missing workflow (generation 1) or
// replaces an existing one as a new generation, both sourced proposal:<id>
// in workflow_generations — the audited changelog rollback restores from.
func TestApplyWorkflowProposal(t *testing.T) {
	f := newApplyFixture(t)
	f.writeDirective("scan-step", "---\nmode: run\nmodel: haiku\n---\nscan\n")

	graph := `{"graph":{"nodes":[{"id":"scan","type":"directive","config":{"directive":"scan-step"}}],"edges":[]},"description":"nightly scan"}`
	p := f.approved(model.ProposalWorkflow, "workflow:nightly-scan", graph)
	ref, err := f.apply(p)
	if err != nil {
		t.Fatal(err)
	}
	if ref != "workflow:nightly-scan@1" {
		t.Fatalf("ref = %s", ref)
	}
	wf, err := f.st.GetWorkflow(actx(), "nightly-scan")
	if err != nil || wf.Generation != 1 || wf.Description != "nightly scan" || len(wf.Graph.Nodes) != 1 {
		t.Fatalf("created workflow = %+v, %v", wf, err)
	}

	// A second proposal replaces the graph as generation 2.
	graph2 := `{"graph":{"nodes":[{"id":"scan","type":"directive","config":{"directive":"scan-step"}},{"id":"scan2","type":"directive","config":{"directive":"scan-step"}}],"edges":[{"from":"scan","to":"scan2"}]}}`
	p2 := f.approved(model.ProposalWorkflow, "workflow:nightly-scan", graph2)
	if ref, err = f.apply(p2); err != nil || ref != "workflow:nightly-scan@2" {
		t.Fatalf("update ref = %s, %v", ref, err)
	}

	// The changelog records both, sourced by their proposals.
	gens, err := f.st.WorkflowGenerations(actx(), "nightly-scan", 10)
	if err != nil || len(gens) != 2 {
		t.Fatalf("generations = %d, %v", len(gens), err)
	}
	if gens[0].Generation != 2 || gens[0].Source != "proposal:"+p2.ID || gens[1].Source != "proposal:"+p.ID {
		t.Fatalf("sources = %+v", gens)
	}

	// A graph naming an unknown directive is refused at apply time.
	bad := f.approved(model.ProposalWorkflow, "workflow:broken", `{"graph":{"nodes":[{"id":"x","type":"directive","config":{"directive":"no-such"}}],"edges":[]}}`)
	if _, err := f.apply(bad); err == nil || !strings.Contains(err.Error(), "not in the library") {
		t.Fatalf("unknown directive accepted: %v", err)
	}
}

// Rollback restores an old generation as a NEW audited generation.
func TestWorkflowRollback(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.createRoutineWith("stepdir", "step {{objective}}")

	wf := &store.Workflow{Name: "wfroll", Graph: &store.WorkflowGraph{
		Nodes: []store.WorkflowNode{{ID: "a", Type: store.NodeDirective, Config: map[string]any{"directive": "stepdir"}}}}}
	h.call("POST", "/api/v1/workflows", wf, nil, 201)
	wf.Graph.Nodes = append(wf.Graph.Nodes, store.WorkflowNode{ID: "b", Type: store.NodeDirective, Config: map[string]any{"directive": "stepdir"}})
	wf.Graph.Edges = []store.WorkflowGraphEdge{{From: "a", To: "b"}}
	h.call("PUT", "/api/v1/workflows/wfroll?generation=1", wf, nil, 200)

	var restored store.Workflow
	h.call("POST", "/api/v1/workflows/wfroll/rollback", map[string]int{"generation": 1}, &restored, 200)
	if restored.Generation != 3 || len(restored.Graph.Nodes) != 1 {
		t.Fatalf("restored = generation %d, %d nodes", restored.Generation, len(restored.Graph.Nodes))
	}
	var gens []store.WorkflowGenerationRow
	h.call("GET", "/api/v1/workflows/wfroll/generations", nil, &gens, 200)
	if len(gens) != 3 || gens[0].Source != "rollback:1" || gens[0].Generation != 3 {
		t.Fatalf("changelog = %+v", gens)
	}
	// Rolling back to the current generation is refused.
	if status, _ := h.do("POST", "/api/v1/workflows/wfroll/rollback", map[string]int{"generation": 3}, nil, testToken); status != 400 {
		t.Fatalf("self-rollback = %d", status)
	}
}
