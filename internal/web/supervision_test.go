package web

import (
	"context"
	"log/slog"
	"strings"
	"testing"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/store"
)

func testSupervisionCfg() config.SupervisionConfig {
	return config.SupervisionConfig{HardCeilingTurns: 200, SoftTurns: 80, SilenceMinutes: 5, SpinWindowTurns: 25, MaxAutoExtensions: 3, DeciderModel: "opus"}
}

// TestClassifyBudget drives the deterministic ladder directly: the pure policy
// with no decider involved.
func TestClassifyBudget(t *testing.T) {
	cfg := testSupervisionCfg()
	ask := &budgetAsk{Dimension: store.BudgetTurns, Amount: 20, Reason: "finishing the refactor"}

	// Grant under the ceiling with evident progress.
	v := classifyBudget(cfg, supervisionEvidence{RunningTurns: 40, ArtifactGrowth: 5, PriorRequests: 0, FirstAskGrowth: -1, GrewSinceLast: true}, ask)
	if v.Action != store.BudgetExtend || v.GrantedAmount != 20 || v.DecidedBy != "policy" {
		t.Errorf("grant: %+v", v)
	}

	// Kill EARLY on diminishing returns driven by the ledger: 3 asks, no growth
	// since the first — do not ride to the ceiling.
	v = classifyBudget(cfg, supervisionEvidence{RunningTurns: 100, ArtifactGrowth: 5, PriorRequests: 3, FirstAskGrowth: 5, GrewSinceLast: false}, ask)
	if v.Action != store.BudgetKill || v.DecidedBy != "policy" || !strings.Contains(v.Rationale, "diminishing returns") {
		t.Errorf("diminishing returns kill: %+v", v)
	}

	// Kill on clear spin: one signature dominates a real window, no new artifacts.
	v = classifyBudget(cfg, supervisionEvidence{RunningTurns: 50, ArtifactGrowth: 2, PriorRequests: 0, FirstAskGrowth: -1, WindowSamples: 20, Dominance: 0.9, GrewSinceLast: false}, nil)
	if v.Action != store.BudgetKill || !strings.Contains(v.Rationale, "spin") {
		t.Errorf("spin kill: %+v", v)
	}

	// Over the hard ceiling is always a kill.
	v = classifyBudget(cfg, supervisionEvidence{RunningTurns: 200}, nil)
	if v.Action != store.BudgetKill || !strings.Contains(v.Rationale, "hard ceiling") {
		t.Errorf("ceiling kill: %+v", v)
	}

	// An ask with no evident progress is ambiguous — escalate, do not kill.
	v = classifyBudget(cfg, supervisionEvidence{RunningTurns: 40, ArtifactGrowth: 5, PriorRequests: 1, FirstAskGrowth: 5, GrewSinceLast: false}, ask)
	if !v.escalate || v.Action == store.BudgetKill {
		t.Errorf("ambiguous ask should escalate: %+v", v)
	}

	// A spin signal on too few samples is NOT a deterministic kill.
	v = classifyBudget(cfg, supervisionEvidence{RunningTurns: 30, WindowSamples: 3, Dominance: 1.0, GrewSinceLast: false, FirstAskGrowth: -1}, nil)
	if v.Action == store.BudgetKill {
		t.Errorf("too-few-samples should not kill: %+v", v)
	}
}

func TestParseSupervisionDecision(t *testing.T) {
	if d := parseSupervisionDecision(`{"action":"kill","rationale":"wedged"}`); d.Action != "kill" || d.Rationale != "wedged" {
		t.Errorf("kill: %+v", d)
	}
	if d := parseSupervisionDecision("Sure:\n```json\n{\"action\":\"extend\",\"amount\":25}\n```"); d.Action != "extend" || d.Amount != 25 {
		t.Errorf("extend: %+v", d)
	}
	// Unparseable / unknown action falls back to continue (never a kill).
	if d := parseSupervisionDecision("no json"); d.Action != "continue" {
		t.Errorf("fallback: %+v", d)
	}
	if d := parseSupervisionDecision(`{"action":"nuke"}`); d.Action != "continue" {
		t.Errorf("unknown action: %+v", d)
	}
}

// TestAdjudicateEscalatesToDecider exercises the ambiguous middle: an injected
// fake modelCall stands in for opus and its verdict is honoured.
func TestAdjudicateEscalatesToDecider(t *testing.T) {
	srv := &Server{Engine: &Engine{log: slog.Default(), now: time.Now, supervisionCfg: testSupervisionCfg()}}
	var sawModel string
	srv.modelCall = func(_ context.Context, _, _, m string) (string, error) {
		sawModel = m
		return `{"action":"kill","rationale":"no progress in a while"}`, nil
	}
	// Silence is ambiguous → escalates.
	ev := supervisionEvidence{AttemptID: "att-1", RunningTurns: 30, Silent: true, SilenceFor: 6 * time.Minute, FirstAskGrowth: -1}
	v := srv.adjudicate(context.Background(), ev, nil)
	if v.Action != store.BudgetKill || v.DecidedBy != "auto:opus" {
		t.Errorf("decider kill: %+v", v)
	}
	if sawModel != "opus" {
		t.Errorf("decider model = %q, want opus", sawModel)
	}

	// A decider extend is bounded and applied.
	srv.modelCall = func(_ context.Context, _, _, _ string) (string, error) {
		return `{"action":"extend","amount":9999,"rationale":"almost done"}`, nil
	}
	ev = supervisionEvidence{AttemptID: "att-1", RunningTurns: 190, Silent: true, FirstAskGrowth: -1}
	v = srv.adjudicate(context.Background(), ev, &budgetAsk{Dimension: store.BudgetTurns, Amount: 9999})
	if v.Action != store.BudgetExtend || v.GrantedAmount != 10 { // bounded by room to the 200 ceiling
		t.Errorf("decider extend bounded: %+v", v)
	}

	// No decider configured → ambiguous resolves to a safe continue, never a kill.
	srv.modelCall = nil
	v = srv.adjudicate(context.Background(), supervisionEvidence{Silent: true, FirstAskGrowth: -1}, nil)
	if v.Action != store.BudgetContinue {
		t.Errorf("no decider should continue: %+v", v)
	}
}
