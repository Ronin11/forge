package stats_test

import (
	"testing"

	"forge/internal/core/model"
	"forge/internal/stats"
	"forge/internal/store"
)

func mfact(modelAlias, class, runner string, state model.State, pass *bool, usd, secs float64) store.AttemptFacts {
	f := store.AttemptFacts{Routine: "r", State: state, Repository: "repo", Project: "default", Mode: "run",
		Model: modelAlias, ModelClass: class, Runner: runner, VerificationPass: pass}
	f.USD = &usd
	f.RunnerSeconds = &secs
	return f
}

func TestCapabilityMatrix(t *testing.T) {
	yes, no := true, false
	rows := []store.AttemptFacts{
		mfact("kimi", "mid", "devbox", model.Succeeded, &yes, 0.10, 30),
		mfact("kimi", "mid", "devbox", model.Failed, &no, 0.08, 20),
		mfact("haiku", "small", "claude", model.Succeeded, &yes, 0.02, 10),
		mfact("haiku", "small", "claude", model.Succeeded, &yes, 0.04, 12),
	}
	rep := stats.Compute(rows, nil)
	if len(rep.Matrix) != 2 {
		t.Fatalf("want 2 model rows, got %d: %+v", len(rep.Matrix), rep.Matrix)
	}
	byModel := map[string]stats.ModelCapability{}
	for _, m := range rep.Matrix {
		byModel[m.Model] = m
	}
	kimi := byModel["kimi"]
	if kimi.Class != "mid" || kimi.Runner != "devbox" || kimi.Runs != 2 || kimi.Trials != 2 || kimi.VerifiedSuccesses != 1 {
		t.Errorf("kimi row: %+v", kimi)
	}
	if kimi.VerifiedSuccessRate != 0.5 {
		t.Errorf("kimi verified rate = %v, want 0.5", kimi.VerifiedSuccessRate)
	}
	// Cost per verified success: only the verified attempt's usd (0.10) / 1.
	if kimi.CostPerVerifiedUSD == nil || *kimi.CostPerVerifiedUSD != 0.10 {
		t.Errorf("kimi cost/verified = %v, want 0.10", kimi.CostPerVerifiedUSD)
	}
	haiku := byModel["haiku"]
	if haiku.VerifiedSuccesses != 2 || haiku.CostPerVerifiedUSD == nil || *haiku.CostPerVerifiedUSD != 0.03 {
		t.Errorf("haiku row: %+v (cost/verified want 0.03)", haiku)
	}
	// Runner utilization: devbox 2 runs (30+20=50s), claude 2 runs (22s).
	util := map[string]stats.RunnerUtilization{}
	for _, u := range rep.Runners {
		util[u.Runner] = u
	}
	if util["devbox"].TotalRunnerSeconds != 50 || util["claude"].TotalRunnerSeconds != 22 {
		t.Errorf("runner utilization: %+v", rep.Runners)
	}
}

// A model with no verified success reports nil cost-per-verified, not zero.
func TestCapabilityMatrixNoVerified(t *testing.T) {
	no := false
	rows := []store.AttemptFacts{mfact("kimi", "mid", "devbox", model.Failed, &no, 0.10, 30)}
	rep := stats.Compute(rows, nil)
	if len(rep.Matrix) != 1 || rep.Matrix[0].CostPerVerifiedUSD != nil {
		t.Errorf("no verified success => nil cost/verified: %+v", rep.Matrix)
	}
}
