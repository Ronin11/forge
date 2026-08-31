package controlplane

import (
	"testing"
	"time"

	"forge/internal/model"
	"forge/internal/protocol"
	"forge/internal/store"
)

// The M10 cost vector: usd is tokens × the model's price; runner_seconds is the
// agent phase in seconds; the window deltas come from bracketing samples;
// runner/class/escalated_from mirror the attempt.
func TestComputeFactsCostVector(t *testing.T) {
	now := time.Date(2026, 8, 31, 12, 0, 0, 0, time.UTC)
	started := now.Add(-time.Minute)
	in := FactsInput{
		Attempt: store.Attempt{
			ID: "a2", WorkerID: "w", Executor: "claude-code", ModelAlias: "kimi", Runner: "devbox", EscalatedFrom: "haiku",
			Mode: "run", Autonomy: model.AutonomyAuto, StartedAt: started, FinishedAt: now,
			Usage:      protocol.Usage{InputTokens: 1_000_000, OutputTokens: 2_000_000, CacheReadTokens: 0, CacheCreationTokens: 0},
			HeadCommit: "", Branch: "",
		},
		Target:  store.Target{ID: "t2", Repository: "app", State: model.Succeeded},
		Work:    store.Work{ID: "w2", RoutineName: "route", Generation: 1, Trigger: model.TriggerManual},
		Project: "default",
		Events: []store.StoredEvent{
			ev("worker", "span_end", "agent-1", "agent-1", "", 1000, 90_000_000, ""), // 90s agent
		},
		Samples: []store.RateLimitSample{
			{Time: started.Add(-time.Minute), Window: "five_hour", Utilization: 0.20, ResetsAt: now.Add(time.Hour)},
			{Time: now, Window: "five_hour", Utilization: 0.35, ResetsAt: now.Add(time.Hour), SourceAttempt: "a2"},
			{Time: started.Add(-time.Minute), Window: "seven_day", Utilization: 0.40, ResetsAt: now.Add(48 * time.Hour)},
			{Time: now, Window: "seven_day", Utilization: 0.42, ResetsAt: now.Add(48 * time.Hour), SourceAttempt: "a2"},
		},
		Now:   now,
		Model: ModelInfo{Alias: "kimi", ID: "kimi-k2", Runner: "devbox", Class: "mid", Price: Price{Input: 2.0, Output: 6.0}},
	}
	f := ComputeFacts(in)
	// usd = (1e6*2 + 2e6*6) / 1e6 = 2 + 12 = 14.
	if f.USD == nil || *f.USD != 14.0 {
		t.Errorf("usd = %v, want 14", f.USD)
	}
	if f.RunnerSeconds == nil || *f.RunnerSeconds != 90.0 {
		t.Errorf("runner_seconds = %v, want 90", f.RunnerSeconds)
	}
	if f.FiveHourDelta == nil || !approxF(*f.FiveHourDelta, 0.15) {
		t.Errorf("five_hour_delta = %v, want 0.15", f.FiveHourDelta)
	}
	if f.SevenDayDelta == nil || !approxF(*f.SevenDayDelta, 0.02) {
		t.Errorf("seven_day_delta = %v, want 0.02", f.SevenDayDelta)
	}
	if f.Runner != "devbox" || f.ModelClass != "mid" || f.EscalatedFrom != "haiku" {
		t.Errorf("routing fields: runner=%q class=%q escalated=%q", f.Runner, f.ModelClass, f.EscalatedFrom)
	}
}

// With no price and no samples, the cost vector stays NULL — honest, not zero.
func TestComputeFactsCostVectorNoPrice(t *testing.T) {
	now := time.Date(2026, 8, 31, 12, 0, 0, 0, time.UTC)
	in := FactsInput{
		Attempt: store.Attempt{ID: "a3", ModelAlias: "kimi", Mode: "run", Autonomy: model.AutonomyAuto, FinishedAt: now,
			Usage: protocol.Usage{InputTokens: 100, OutputTokens: 200}},
		Target:  store.Target{ID: "t3", Repository: "app", State: model.Failed},
		Work:    store.Work{ID: "w3", RoutineName: "route", Trigger: model.TriggerManual},
		Project: "default", Now: now,
		Model: ModelInfo{Alias: "kimi"}, // no price
	}
	f := ComputeFacts(in)
	if f.USD != nil {
		t.Errorf("usd should be NULL without a price, got %v", *f.USD)
	}
	if f.FiveHourDelta != nil || f.SevenDayDelta != nil {
		t.Error("window deltas should be NULL without bracketing samples")
	}
}

func approxF(a, b float64) bool {
	d := a - b
	if d < 0 {
		d = -d
	}
	return d < 1e-9
}
