package engine

import (
	"fmt"
	"forge/internal/core/config"
	"math"
	mrand "math/rand/v2"
	"sort"
)

// router.go is M10's model chooser (DESIGN.md §21). At claim time, for a target,
// it filters the candidate models to those whose runner is ready and has
// capacity and whose verified-success record clears a Wilson lower-bound gate
// (once enough samples exist), then picks the lowest-scoring candidate by the
// weighted cost-vector estimate. It is a pure function: the caller assembles
// the evidence from facts and config so the decision is deterministic and
// testable, seeding the RNG per target id so exploration reproduces.

// CostVector is one attempt's four cost terms: notional dollars, the two
// subscription-window deltas, and agent wall seconds (DESIGN.md §21). The
// router scores a candidate by the weighted sum of its estimate.
type CostVector struct {
	USD           float64 `json:"usd"`
	FiveHour      float64 `json:"five_hour"`
	SevenDay      float64 `json:"seven_day"`
	RunnerSeconds float64 `json:"runner_seconds"`
}

// ModelEvidence is what the router knows about one candidate model for a
// routine: its p50 cost-vector estimate (already fallen back from routine+model
// to tier/model to the configured price by the caller) and its verified-success
// record (Successes of Trials).
type ModelEvidence struct {
	Estimate  CostVector
	Successes int
	Trials    int
}

// RouteInput is everything Route needs; the caller builds it from the routine,
// the config, live runner health, and facts.
type RouteInput struct {
	TargetID  string
	Routine   string
	Tier      int
	Allowlist []string                    // routine.Models; empty means "all models with max_tier ≥ tier"
	Models    map[string]config.ModelInfo // every configured alias → its info
	AllModels []string                    // sorted configured aliases (candidate set when Allowlist empty)

	RunnerReady func(runner string) bool // runner:<name> == ready
	RunnerFree  func(runner string) bool // a runner slot is free
	Evidence    func(alias string) ModelEvidence

	Weights            config.Weights
	MinVerifiedSuccess float64
	MinSamples         int
	Explore            float64
	Rand               *mrand.Rand // seeded per target id; nil uses an internal seed
}

// RouteCandidate is one model the router considered, with the reason it was
// kept or dropped — recorded on the attempt for the task-detail UI.
type RouteCandidate struct {
	Model     string  `json:"model"`
	Runner    string  `json:"runner"`
	Class     string  `json:"class"`
	Score     float64 `json:"score"`
	WilsonLB  float64 `json:"wilson_lb"`
	Successes int     `json:"successes"`
	Trials    int     `json:"trials"`
	Eligible  bool    `json:"eligible"`
	Reason    string  `json:"reason"` // why kept ("scored", "explore") or dropped
}

// RoutingDecision is what Route returns and what the attempt stores in its
// `routing` JSON column (DESIGN.md §21): the candidates, their scores, the
// chosen model, and a one-line why.
type RoutingDecision struct {
	Chosen     string           `json:"chosen"`
	Tier       int              `json:"tier"`
	Why        string           `json:"why"`
	Candidates []RouteCandidate `json:"candidates"`
}

// wilsonZ is the 95% two-sided normal quantile: the Wilson lower bound uses it
// so a model with few samples is trusted only once its successes hold up.
const wilsonZ = 1.96

// wilsonLowerBound is the Wilson score interval's lower bound of the success
// probability for s successes in n trials. n==0 returns 0 (no evidence, no
// trust). The bound is conservative for small n, which is exactly what gates a
// barely-sampled model.
func wilsonLowerBound(s, n int) float64 {
	if n <= 0 {
		return 0
	}
	phat := float64(s) / float64(n)
	z2 := wilsonZ * wilsonZ
	denom := 1 + z2/float64(n)
	center := phat + z2/(2*float64(n))
	margin := wilsonZ * math.Sqrt(phat*(1-phat)/float64(n)+z2/(4*float64(n)*float64(n)))
	lb := (center - margin) / denom
	if lb < 0 {
		return 0
	}
	return lb
}

// weightScore is the weighted cost-vector estimate the router minimises
// (a free function: Weights lives in core/config, so it cannot carry the
// router's method).
func weightScore(w config.Weights, v CostVector) float64 {
	return w.USD*v.USD + w.FiveHour*v.FiveHour + w.SevenDay*v.SevenDay + w.RunnerSeconds*v.RunnerSeconds
}

// Route chooses the model for a target and returns the full decision. ok is
// false when no candidate can run right now (every candidate's runner is down
// or at capacity), so the caller skips the target and lets the scheduler retry.
func Route(in RouteInput) (RoutingDecision, bool) {
	rng := in.Rand
	if rng == nil {
		rng = mrand.New(mrand.NewPCG(SeedFromID(in.TargetID), 0x10))
	}
	aliases := candidateAliases(in)
	dec := RoutingDecision{Tier: in.Tier}
	runnable := 0 // candidates whose runner is ready and free (gate aside)
	for idx, alias := range aliases {
		info, known := in.Models[alias]
		if !known {
			dec.Candidates = append(dec.Candidates, RouteCandidate{Model: alias, Eligible: false, Reason: "unknown model"})
			continue
		}
		c := RouteCandidate{Model: alias, Runner: info.Runner, Class: info.Class}
		ev := in.Evidence(alias)
		c.Successes, c.Trials = ev.Successes, ev.Trials
		c.WilsonLB = wilsonLowerBound(ev.Successes, ev.Trials)
		c.Score = weightScore(in.Weights, ev.Estimate)
		switch {
		case in.RunnerReady != nil && !in.RunnerReady(info.Runner):
			c.Reason = "runner " + info.Runner + " not ready"
		case in.RunnerFree != nil && !in.RunnerFree(info.Runner):
			c.Reason = "runner " + info.Runner + " at capacity"
		default:
			runnable++
			c.Eligible, c.Reason = gate(c, ev, in, idx, rng)
		}
		dec.Candidates = append(dec.Candidates, c)
	}
	if runnable == 0 {
		return dec, false
	}
	choose(&dec, aliases)
	return dec, dec.Chosen != ""
}

// gate applies the Wilson success gate and the exploration rule to a
// runner-runnable candidate, returning eligibility and the reason.
func gate(c RouteCandidate, ev ModelEvidence, in RouteInput, ladderIdx int, rng *mrand.Rand) (bool, string) {
	if ev.Trials >= in.MinSamples {
		if c.WilsonLB < in.MinVerifiedSuccess {
			return false, fmt.Sprintf("verified success %.2f below gate %.2f", c.WilsonLB, in.MinVerifiedSuccess)
		}
		return true, "scored"
	}
	// Below the sample floor a model is unproven: eligible only with probability
	// explore. The RNG is seeded per target id, so the roll reproduces in tests.
	if rng.Float64() < in.Explore {
		return true, "explore"
	}
	return false, fmt.Sprintf("unproven (%d/%d < min_samples %d), explore skipped", ev.Trials, ev.Trials, in.MinSamples)
}

// choose picks the lowest-scoring eligible candidate; ties break by ladder
// order (the Allowlist / sorted-alias order in aliases) then by name. When no
// candidate is eligible but some are runnable, it forces the lowest-scoring
// runnable one — a routine must still make progress even before any model has
// earned the gate, or when its only models are all under the gate.
func choose(dec *RoutingDecision, aliases []string) {
	order := map[string]int{}
	for i, a := range aliases {
		order[a] = i
	}
	pick := func(pred func(RouteCandidate) bool) (RouteCandidate, bool) {
		var best RouteCandidate
		found := false
		for _, c := range dec.Candidates {
			if !pred(c) {
				continue
			}
			if !found || c.Score < best.Score || (c.Score == best.Score && order[c.Model] < order[best.Model]) {
				best, found = c, true
			}
		}
		return best, found
	}
	if best, ok := pick(func(c RouteCandidate) bool { return c.Eligible }); ok {
		dec.Chosen = best.Model
		dec.Why = fmt.Sprintf("lowest score %.4f among eligible (%s)", best.Score, best.Reason)
		return
	}
	if best, ok := pick(func(c RouteCandidate) bool { return c.Reason != "" && !isRunnerReason(c.Reason) && c.Runner != "" }); ok {
		dec.Chosen = best.Model
		dec.Why = "no candidate cleared the gate; forced lowest-score runnable"
		return
	}
}

func isRunnerReason(r string) bool {
	return len(r) >= 7 && r[:7] == "runner " || r == "unknown model"
}

// candidateAliases is the ordered candidate set: the routine's allowlist when
// present (the ladder order is preserved), else every configured model whose
// max_tier reaches the routine's tier, sorted.
func candidateAliases(in RouteInput) []string {
	if len(in.Allowlist) > 0 {
		return in.Allowlist
	}
	var out []string
	for _, a := range in.AllModels {
		if info, ok := in.Models[a]; ok && info.MaxTier >= in.Tier {
			out = append(out, a)
		}
	}
	sort.Strings(out)
	return out
}

// SeedFromID turns a target id (hex) into a stable uint64 seed so exploration
// is reproducible per target.
func SeedFromID(id string) uint64 {
	var h uint64 = 1469598103934665603 // FNV-1a offset basis
	for i := 0; i < len(id); i++ {
		h ^= uint64(id[i])
		h *= 1099511628211
	}
	return h
}
