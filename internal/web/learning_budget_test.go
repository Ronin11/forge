package web

import (
	"context"
	"net/http"
	"strings"
	"testing"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/model"
	"forge/internal/core/store"
)

// The learning ledger: reflect-library roots are refused once the rolling-7d
// self-improvement spend crosses [learning] usd_per_week; project work and
// mid-flight children are untouched.
func TestLearningBudgetGate(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutineWith("reflect-library", "reflect {{objective}}")
	h.createRoutineWith("normal-work", "work {{objective}}")
	h.srv.learningCfg = config.LearningConfig{BudgetUSDPerWeek: 1.0}

	// Under budget: a reflect run is created.
	first := h.run("reflect-library")

	// Burn the pool: a finished reflect attempt that cost more than the week's
	// budget, inside the window.
	c := h.mustClaim("req-lb")
	cost := 1.50
	if err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		return tx.InsertFacts(context.Background(), &store.AttemptFacts{
			AttemptID: c.AttemptID, TargetID: c.TargetID, WorkID: first.Work.ID, Routine: "reflect-library",
			Project: "default", Repository: "equitizr", Worker: testWorkerID, Executor: "claude-code",
			Model: "haiku", Mode: "run", Trigger: model.TriggerManual, Autonomy: model.AutonomyAuto,
			FinishedAt: h.clock.Now().Add(-time.Hour), State: model.Succeeded, CostUSD: &cost})
	}); err != nil {
		t.Fatal(err)
	}

	status, body := h.do(http.MethodPost, "/api/v1/routines/reflect-library/run", nil, nil, testToken)
	if status != http.StatusConflict || !strings.Contains(string(body), "learning budget exhausted") {
		t.Fatalf("over-budget reflect = %d %s", status, body)
	}
	// Project work is not gated by the learning pool.
	h.run("normal-work")

	// Ledger off (zero budget) lifts the gate.
	h.srv.learningCfg = config.LearningConfig{}
	h.run("reflect-library")
}
