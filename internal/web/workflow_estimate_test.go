package web

import (
	"context"
	"fmt"
	"net/http"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// estimateFact seeds one attempt-facts row carrying a notional USD figure for
// (routine, model alias) — the history the estimate prices from. A real
// work/claim pair backs the row's foreign keys.
func estimateFact(t *testing.T, h *harness, seq int, routine, alias string, usd float64) {
	t.Helper()
	ctx := context.Background()
	req := fmt.Sprintf("est-%d", seq)
	w := &store.Work{RoutineName: routine, Generation: 1, Title: "est " + req, Trigger: model.TriggerManual,
		Snapshot: []byte(`{}`), Priority: 100, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto}
	var targets []store.Target
	var a *store.Attempt
	err := h.srv.store.Write(ctx, func(tx *store.Tx) error {
		var err error
		if targets, err = tx.CreateWork(ctx, w, []string{"equitizr"}, nil); err != nil {
			return err
		}
		if a, err = tx.Claim(ctx, store.ClaimParams{TargetID: targets[0].ID, WorkerID: testWorkerID, ClaimRequestID: req,
			LeaseToken: "lease-" + req, MCPToken: "mcp-" + req, Executor: "claude-code", Model: "m-" + alias,
			ModelAlias: alias, Mode: "run", Autonomy: model.AutonomyAuto}); err != nil {
			return err
		}
		pass := true
		return tx.InsertFacts(ctx, &store.AttemptFacts{AttemptID: a.ID, TargetID: a.TargetID, WorkID: w.ID,
			Routine: routine, Generation: 1, Project: "default", Repository: "equitizr", Worker: testWorkerID,
			Executor: "claude-code", Model: alias, Mode: "run", Trigger: model.TriggerManual, Autonomy: model.AutonomyAuto,
			FinishedAt: h.clock.Now(), State: model.Succeeded, VerificationPass: &pass, USD: &usd})
	})
	if err != nil {
		t.Fatal(err)
	}
}

// The estimate prices routine nodes from history — routine+model narrowed,
// model-wide fallback, honest "none" when nothing is known — flags branching
// and loop caps, and skips script/switch nodes entirely.
func TestWorkflowEstimate(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{
		"personas/builder.md": "---\nmodel: sonnet\n---\nYou build.",
	})

	for _, rt := range []store.Routine{
		{Name: "wfe-lint", Mode: "run", Prompt: "lint", Model: "haiku", Repositories: []string{"equitizr"}, TimeoutSeconds: 300},
		{Name: "wfe-fix", Mode: "run", Prompt: "fix", Persona: "builder", Repositories: []string{"equitizr"}, TimeoutSeconds: 300},
		{Name: "wfe-mystery", Mode: "run", Prompt: "?", Model: "opus", Repositories: []string{"equitizr"}, TimeoutSeconds: 300},
	} {
		h.call(http.MethodPost, "/api/v1/routines", rt, nil, http.StatusCreated)
	}

	// wfe-lint on haiku has direct history (p50 of 0.10/0.20/0.30 = 0.20);
	// sonnet has only model-wide history from an unrelated routine (1.00);
	// opus has none.
	estimateFact(t, h, 1, "wfe-lint", "haiku", 0.10)
	estimateFact(t, h, 2, "wfe-lint", "haiku", 0.20)
	estimateFact(t, h, 3, "wfe-lint", "haiku", 0.30)
	estimateFact(t, h, 4, "wfe-other", "sonnet", 1.00)

	graph := map[string]any{
		"nodes": []map[string]any{
			{"id": "lint", "type": "routine", "config": map[string]any{"routine": "wfe-lint"}, "position": map[string]float64{"x": 0, "y": 0}},
			{"id": "route", "type": "switch", "config": map[string]any{"expression": "'go'"}, "position": map[string]float64{"x": 200, "y": 0}},
			{"id": "fix", "type": "routine", "config": map[string]any{"routine": "wfe-fix"}, "position": map[string]float64{"x": 400, "y": 0}},
			{"id": "mystery", "type": "routine", "config": map[string]any{"routine": "wfe-mystery"}, "position": map[string]float64{"x": 600, "y": 0}},
		},
		"edges": []map[string]any{
			{"from": "lint", "to": "route", "when": "success"},
			{"from": "route", "to": "fix", "when": "case", "case": "go", "default": true},
			{"from": "fix", "to": "mystery", "when": "success"},
			{"from": "mystery", "to": "fix", "when": "failure", "loop": true, "max_iterations": 3},
		},
	}
	h.call(http.MethodPost, "/api/v1/workflows", map[string]any{"name": "wfe", "graph": graph}, nil, http.StatusCreated)

	var est workflowEstimate
	h.call(http.MethodGet, "/api/v1/workflows/wfe/estimate", nil, &est, http.StatusOK)
	if est.RoutineNodes != 3 || est.KnownNodes != 2 || !est.Conditional {
		t.Fatalf("estimate = %+v", est)
	}
	if est.KnownUSD < 1.19 || est.KnownUSD > 1.21 {
		t.Errorf("known usd = %v, want 1.20", est.KnownUSD)
	}
	byNode := map[string]workflowNodeEstimate{}
	for _, n := range est.Nodes {
		byNode[n.Node] = n
	}
	lint := byNode["lint"]
	if lint.Model != "haiku" || lint.Source != "routine" || lint.Samples != 3 || lint.USD == nil || *lint.USD != 0.20 {
		t.Errorf("lint = %+v", lint)
	}
	// wfe-fix's model comes from its persona; its price from model-wide facts.
	fix := byNode["fix"]
	if fix.Model != "sonnet" || fix.Source != "model" || fix.USD == nil || *fix.USD != 1.00 || fix.LoopCap != 3 {
		t.Errorf("fix = %+v", fix)
	}
	mystery := byNode["mystery"]
	if mystery.Source != "none" || mystery.USD != nil || mystery.Model != "opus" {
		t.Errorf("mystery = %+v", mystery)
	}
}
