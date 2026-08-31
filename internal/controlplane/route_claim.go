package controlplane

import (
	"context"
	"encoding/json"
	"fmt"
	mrand "math/rand/v2"
	"sort"
	"strings"
	"time"

	"forge/internal/store"
)

// route_claim.go binds the pure router (router.go) to the claim transaction
// (DESIGN.md §21): it resolves the routine's candidate models, gathers facts
// evidence and live runner health, chooses the model, and climbs the
// escalation ladder after a prior attempt failed verification.

// routingWindow bounds how far back routing looks for cost/success evidence.
const routingWindow = 30 * 24 * time.Hour

// routeChoice is the resolved model for one claim.
type routeChoice struct {
	Alias         string
	ModelID       string
	Runner        string
	Executor      string
	EscalatedFrom string
	RoutingJSON   string
}

// modelInfoFor resolves an alias, using the config-backed table when wired and
// falling back to the plain resolveModel seam (M1) otherwise.
func (s *Server) modelInfoFor(alias string) (ModelInfo, bool) {
	if s.modelInfo != nil {
		return s.modelInfo(alias)
	}
	id, ok := s.resolveModel(alias)
	return ModelInfo{Alias: alias, ID: id}, ok
}

// routeClaim decides the model for a claim. Routing engages only when the model
// table is wired and the routine opts in (a models allowlist or an explicit
// tier); otherwise it is the M1 single-model path. On a retry after a failed
// attempt, an allowlist routine escalates to the next ladder rung. Returns
// nil,nil when routing found no runner able to run any candidate right now, so
// the claim is skipped and retried.
func (s *Server) routeClaim(ctx context.Context, worker store.Worker, w store.Work, t store.Target, snap store.Routine, tx *store.Tx) (*routeChoice, error) {
	allowlist := snap.Models
	tier := 0
	if snap.Tier != nil {
		tier = *snap.Tier
	}
	if s.modelInfo == nil || (len(allowlist) == 0 && snap.Tier == nil) {
		info, ok := s.modelInfoFor(snap.Model)
		if !ok {
			return nil, fmt.Errorf("work %s: unknown model alias %q", w.ID, snap.Model)
		}
		return &routeChoice{Alias: snap.Model, ModelID: info.ID, Runner: info.Runner, Executor: executorFor(info, snap)}, nil
	}

	priors, err := s.store.AttemptsForTarget(ctx, t.ID)
	if err != nil {
		return nil, err
	}
	priorCount, prevAlias := 0, ""
	for _, a := range priors {
		if !a.FinishedAt.IsZero() {
			priorCount++
			prevAlias = a.ModelAlias
		}
	}

	// Escalation: an allowlist routine climbs the ladder deterministically after
	// a prior finished (failed) attempt, injecting that failure into the prompt
	// downstream (assembleClaimPrompt) and recording escalated_from.
	if len(allowlist) > 0 && priorCount > 0 {
		rung := priorCount
		if rung > len(allowlist)-1 {
			rung = len(allowlist) - 1
		}
		alias := allowlist[rung]
		info, ok := s.modelInfoFor(alias)
		if !ok {
			return nil, fmt.Errorf("routine %s: escalation model %q unknown", snap.Name, alias)
		}
		if !runnerReady(worker, info.Runner) || !s.runnerFree(ctx, tx, info.Runner) {
			return nil, nil
		}
		dec := RoutingDecision{Chosen: alias, Tier: tier, Why: fmt.Sprintf("escalated from %s to ladder rung %d (%s)", prevAlias, rung, alias)}
		return &routeChoice{Alias: alias, ModelID: info.ID, Runner: info.Runner, Executor: executorFor(info, snap), EscalatedFrom: prevAlias, RoutingJSON: mustJSON(dec)}, nil
	}

	in := RouteInput{
		TargetID: t.ID, Routine: w.RoutineName, Tier: tier, Allowlist: allowlist,
		Models: s.modelTable(), AllModels: s.modelAliases,
		RunnerReady: func(r string) bool { return runnerReady(worker, r) },
		RunnerFree:  func(r string) bool { return s.runnerFree(ctx, tx, r) },
		Evidence:    s.buildEvidence(ctx, w.RoutineName),
		Weights:     s.routing.Weights, MinVerifiedSuccess: s.routing.MinVerifiedSuccess,
		MinSamples: s.routing.MinSamples, Explore: s.routing.Explore,
		Rand: mrand.New(mrand.NewPCG(seedFromID(t.ID), 0x10)),
	}
	dec, ok := Route(in)
	if !ok {
		return nil, nil
	}
	info, known := s.modelInfoFor(dec.Chosen)
	if !known {
		return nil, fmt.Errorf("routine %s: chosen model %q unknown", snap.Name, dec.Chosen)
	}
	return &routeChoice{Alias: dec.Chosen, ModelID: info.ID, Runner: info.Runner, Executor: executorFor(info, snap), RoutingJSON: mustJSON(dec)}, nil
}

// modelTable is the full alias→info map the router filters over.
func (s *Server) modelTable() map[string]ModelInfo {
	out := make(map[string]ModelInfo, len(s.modelAliases))
	for _, a := range s.modelAliases {
		if info, ok := s.modelInfoFor(a); ok {
			out[a] = info
		}
	}
	return out
}

// runnerReady reports whether a worker can run a runner right now. A worker
// that advertises runner:<name> gates on its state; a legacy worker that
// advertises no runner capabilities at all is treated as ready for every
// runner (M1 compatibility). A worker that advertises some runner caps but not
// this one is treated as not ready.
func runnerReady(worker store.Worker, runner string) bool {
	if state, ok := worker.Capabilities["runner:"+runner]; ok {
		return state == "ready"
	}
	for k := range worker.Capabilities {
		if strings.HasPrefix(k, "runner:") {
			return false
		}
	}
	return true
}

// runnerFree reports whether the runner has a free capacity slot. Capacity 0 is
// unbounded (bounded only by worker slots — the claude runner). The in-flight
// count is read inside the claim transaction so it is race-free.
func (s *Server) runnerFree(ctx context.Context, tx *store.Tx, runner string) bool {
	limit := s.runnerCap(runner)
	if limit <= 0 {
		return true
	}
	inflight, err := tx.InFlightByRunner(ctx)
	if err != nil {
		s.log.WarnContext(ctx, "in-flight by runner", "error", err)
		return true // fail open: never wedge a claim on a stats read
	}
	return inflight[runner] < limit
}

// runnerCap is the configured capacity for a runner; 0 when unknown/unbounded.
func (s *Server) runnerCap(runner string) int {
	if s.runnerCapacities == nil {
		return 0
	}
	return s.runnerCapacities[runner]
}

// executorFor is the executor a chosen model runs under: the model's own
// executor when it declares one, else the routine's.
func executorFor(info ModelInfo, snap store.Routine) string {
	if info.Executor != "" {
		return info.Executor
	}
	return snap.Executor
}

// buildEvidence returns the router's per-model evidence function for one
// routine: p50 cost vector and verified-success record from facts, falling
// back from routine+model to model-wide facts to the configured price proxy.
func (s *Server) buildEvidence(ctx context.Context, routine string) func(string) ModelEvidence {
	until := s.now().Add(time.Hour)
	since := s.now().Add(-routingWindow)
	routineFacts, err := s.store.FactsSince(ctx, since, until, routine)
	if err != nil {
		s.log.WarnContext(ctx, "routing evidence: routine facts", "error", err)
	}
	globalFacts, err := s.store.FactsSince(ctx, since, until, "")
	if err != nil {
		s.log.WarnContext(ctx, "routing evidence: global facts", "error", err)
	}
	byRoutineModel := groupFactsByModel(routineFacts)
	byModel := groupFactsByModel(globalFacts)
	return func(alias string) ModelEvidence {
		info, _ := s.modelInfoFor(alias)
		ev := ModelEvidence{}
		rows := byRoutineModel[alias]
		if len(rows) == 0 {
			rows = byModel[alias]
		}
		ev.Estimate = estimateVector(rows, info.Price)
		// The success gate is routine-specific: a model must earn trust on THIS
		// routine, not borrow it from an easier one.
		ev.Successes, ev.Trials = successRecord(byRoutineModel[alias])
		return ev
	}
}

func groupFactsByModel(facts []store.AttemptFacts) map[string][]store.AttemptFacts {
	out := map[string][]store.AttemptFacts{}
	for _, f := range facts {
		out[f.Model] = append(out[f.Model], f)
	}
	return out
}

// estimateVector is the p50 cost vector over facts rows, falling back to the
// configured price (input+output, a monotone ordering proxy) for the usd term
// when no rows carry a usd figure.
func estimateVector(rows []store.AttemptFacts, price Price) CostVector {
	usd := p50OfPtr(rows, func(f store.AttemptFacts) *float64 { return f.USD })
	if usd == nil {
		proxy := price.Input + price.Output
		usd = &proxy
	}
	v := CostVector{USD: *usd}
	if d := p50OfPtr(rows, func(f store.AttemptFacts) *float64 { return f.FiveHourDelta }); d != nil {
		v.FiveHour = *d
	}
	if d := p50OfPtr(rows, func(f store.AttemptFacts) *float64 { return f.SevenDayDelta }); d != nil {
		v.SevenDay = *d
	}
	if d := p50OfPtr(rows, func(f store.AttemptFacts) *float64 { return f.RunnerSeconds }); d != nil {
		v.RunnerSeconds = *d
	}
	return v
}

// successRecord is (verified successes, decided trials) over facts rows: a
// trial is any attempt whose verification was decided; a success is a decided
// pass. Undecided attempts do not count either way.
func successRecord(rows []store.AttemptFacts) (successes, trials int) {
	for _, f := range rows {
		if f.VerificationPass == nil {
			continue
		}
		trials++
		if *f.VerificationPass {
			successes++
		}
	}
	return successes, trials
}

func p50OfPtr(rows []store.AttemptFacts, get func(store.AttemptFacts) *float64) *float64 {
	var xs []float64
	for _, f := range rows {
		if v := get(f); v != nil {
			xs = append(xs, *v)
		}
	}
	if len(xs) == 0 {
		return nil
	}
	sort.Float64s(xs)
	v := xs[len(xs)/2]
	return &v
}

func mustJSON(v any) string {
	b, err := json.Marshal(v)
	if err != nil {
		return ""
	}
	return string(b)
}
