package engine

import (
	"forge/internal/core/config"
	"math"
	mrand "math/rand/v2"
	"testing"
)

func testModels() map[string]config.ModelInfo {
	return map[string]config.ModelInfo{
		"haiku":  {Alias: "haiku", ID: "h", Runner: "claude", Class: "small", MaxTier: 3},
		"sonnet": {Alias: "sonnet", ID: "s", Runner: "claude", Class: "mid", MaxTier: 3},
		"opus":   {Alias: "opus", ID: "o", Runner: "claude", Class: "frontier", MaxTier: 3},
		"kimi":   {Alias: "kimi", ID: "k", Runner: "devbox", Class: "mid", MaxTier: 1},
	}
}

func baseInput() RouteInput {
	return RouteInput{
		TargetID:           "0123456789abcdef0123456789abcdef",
		Tier:               0,
		Models:             testModels(),
		AllModels:          []string{"haiku", "kimi", "opus", "sonnet"},
		RunnerReady:        func(string) bool { return true },
		RunnerFree:         func(string) bool { return true },
		Weights:            config.Weights{USD: 1},
		MinVerifiedSuccess: 0.6,
		MinSamples:         5,
		Explore:            0.1,
		Rand:               mrand.New(mrand.NewPCG(1, 2)),
	}
}

// A model whose runner is down or at capacity is ineligible with a runner
// reason; an allowlist model too low for the tier is still honoured (explicit).
func TestRouteRunnerFiltering(t *testing.T) {
	in := baseInput()
	in.Allowlist = []string{"kimi", "haiku"}
	in.RunnerReady = func(r string) bool { return r != "devbox" } // devbox down
	in.Evidence = func(string) ModelEvidence { return ModelEvidence{Estimate: CostVector{USD: 1}} }
	dec, ok := Route(in)
	if !ok || dec.Chosen != "haiku" {
		t.Fatalf("want haiku when devbox down, got %q ok=%v: %+v", dec.Chosen, ok, dec.Candidates)
	}
	for _, c := range dec.Candidates {
		if c.Model == "kimi" && (c.Eligible || c.Reason != "runner devbox not ready") {
			t.Errorf("kimi should be down: %+v", c)
		}
	}
	// Now devbox up but full.
	in.RunnerReady = func(string) bool { return true }
	in.RunnerFree = func(r string) bool { return r != "devbox" }
	dec, ok = Route(in)
	if !ok || dec.Chosen != "haiku" {
		t.Fatalf("want haiku when devbox full, got %q: %+v", dec.Chosen, dec.Candidates)
	}
	// All runners down → not ok.
	in.RunnerReady = func(string) bool { return false }
	in.RunnerFree = func(string) bool { return true }
	if _, ok := Route(in); ok {
		t.Error("want ok=false when every runner down")
	}
}

// max_tier filters the implicit candidate set (no allowlist).
func TestRouteMaxTierFilter(t *testing.T) {
	in := baseInput()
	in.Tier = 2 // kimi has max_tier 1 → excluded
	in.Evidence = func(a string) ModelEvidence {
		cost := map[string]float64{"haiku": 1, "sonnet": 3, "opus": 15}[a]
		return ModelEvidence{Estimate: CostVector{USD: cost}}
	}
	dec, ok := Route(in)
	if !ok {
		t.Fatal("expected a choice")
	}
	for _, c := range dec.Candidates {
		if c.Model == "kimi" {
			t.Errorf("kimi (max_tier 1) must not be a candidate at tier 2: %+v", dec.Candidates)
		}
	}
	if dec.Chosen != "haiku" { // cheapest of the tier-2-eligible
		t.Errorf("want cheapest haiku, got %q", dec.Chosen)
	}
}

// Below min_samples a model is gated behind explore; above it, the Wilson lower
// bound decides.
func TestRouteWilsonGate(t *testing.T) {
	in := baseInput()
	in.Allowlist = []string{"kimi", "haiku"}
	// kimi: 8/20 verified (40%) → Wilson LB well below 0.6; haiku: 19/20.
	in.Evidence = func(a string) ModelEvidence {
		switch a {
		case "kimi":
			return ModelEvidence{Estimate: CostVector{USD: 1}, Successes: 8, Trials: 20}
		default:
			return ModelEvidence{Estimate: CostVector{USD: 5}, Successes: 19, Trials: 20}
		}
	}
	dec, ok := Route(in)
	if !ok || dec.Chosen != "haiku" {
		t.Fatalf("failing kimi should yield haiku, got %q: %+v", dec.Chosen, dec.Candidates)
	}
	for _, c := range dec.Candidates {
		if c.Model == "kimi" && c.Eligible {
			t.Errorf("kimi at 40%% must be gated: %+v", c)
		}
	}
	// Raise kimi's record above the gate: it is cheaper, so it wins.
	in.Evidence = func(a string) ModelEvidence {
		switch a {
		case "kimi":
			return ModelEvidence{Estimate: CostVector{USD: 1}, Successes: 19, Trials: 20}
		default:
			return ModelEvidence{Estimate: CostVector{USD: 5}, Successes: 19, Trials: 20}
		}
	}
	dec, _ = Route(in)
	if dec.Chosen != "kimi" {
		t.Fatalf("proven cheaper kimi should win, got %q: %+v", dec.Chosen, dec.Candidates)
	}
}

// Exploration is deterministic for a given target id: the same seed yields the
// same eligibility for an unproven model across runs.
func TestRouteExploreDeterminism(t *testing.T) {
	build := func() RouteInput {
		in := baseInput()
		in.Allowlist = []string{"kimi"}
		in.Explore = 0.5
		in.Evidence = func(string) ModelEvidence { return ModelEvidence{Estimate: CostVector{USD: 1}} } // 0 trials
		in.Rand = mrand.New(mrand.NewPCG(SeedFromID(in.TargetID), 0x10))
		return in
	}
	first := explored(Route(build()))
	for i := 0; i < 20; i++ {
		if explored(Route(build())) != first {
			t.Fatal("explore roll not reproducible for a fixed target id")
		}
	}
	// A high explore always tries the unproven model; a zero explore never does.
	always := baseInput()
	always.Allowlist = []string{"kimi"}
	always.Explore = 1
	always.Evidence = func(string) ModelEvidence { return ModelEvidence{} }
	if dec, ok := Route(always); !ok || dec.Chosen != "kimi" {
		t.Errorf("explore=1 must try the unproven model: %+v", dec)
	}
	never := baseInput()
	never.Allowlist = []string{"kimi"}
	never.Explore = 0
	never.Evidence = func(string) ModelEvidence { return ModelEvidence{} }
	dec, ok := never2(Route(never))
	// explore=0, unproven, single model → forced runnable (progress must be made).
	if !ok || dec.Chosen != "kimi" {
		t.Errorf("a single unproven model must still run (forced): %+v", dec)
	}
	for _, c := range dec.Candidates {
		if c.Model == "kimi" && c.Eligible {
			t.Errorf("explore=0 should leave kimi ineligible though forced: %+v", c)
		}
	}
}

// Score ordering: lowest weighted cost vector wins; weights select the term.
func TestRouteScoreOrdering(t *testing.T) {
	in := baseInput()
	in.Allowlist = []string{"haiku", "sonnet", "opus"}
	in.MinSamples = 0 // everyone proven enough
	in.Evidence = func(a string) ModelEvidence {
		v := map[string]CostVector{
			"haiku":  {USD: 2, RunnerSeconds: 100},
			"sonnet": {USD: 5, RunnerSeconds: 10},
			"opus":   {USD: 9, RunnerSeconds: 5},
		}[a]
		return ModelEvidence{Estimate: v, Successes: 10, Trials: 10}
	}
	if dec, _ := Route(in); dec.Chosen != "haiku" {
		t.Errorf("usd-weighted should pick haiku, got %q", dec.Chosen)
	}
	in.Weights = config.Weights{RunnerSeconds: 1}
	if dec, _ := Route(in); dec.Chosen != "opus" {
		t.Errorf("runner-seconds-weighted should pick opus, got %q", dec.Chosen)
	}
}

func TestWilsonLowerBound(t *testing.T) {
	if got := wilsonLowerBound(0, 0); got != 0 {
		t.Errorf("no trials => 0, got %v", got)
	}
	// 40% of 10 is well under 0.6; 90% of 10 is above.
	if lb := wilsonLowerBound(4, 10); lb >= 0.6 {
		t.Errorf("4/10 LB %v should be < 0.6", lb)
	}
	if lb := wilsonLowerBound(9, 10); lb < 0.55 || lb > 1 {
		t.Errorf("9/10 LB %v out of expected range", lb)
	}
	// Monotone in successes.
	if wilsonLowerBound(6, 10) <= wilsonLowerBound(5, 10) {
		t.Error("Wilson LB should rise with successes")
	}
	if math.IsNaN(wilsonLowerBound(10, 10)) {
		t.Error("perfect record must not be NaN")
	}
}

func explored(dec RoutingDecision, _ bool) bool {
	for _, c := range dec.Candidates {
		if c.Model == "kimi" {
			return c.Eligible
		}
	}
	return false
}

func never2(dec RoutingDecision, ok bool) (RoutingDecision, bool) { return dec, ok }
