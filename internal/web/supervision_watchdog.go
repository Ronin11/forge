package web

// Phase B — the supervisor-initiated watchdog: a sibling sweep to RunAttention
// that reads each running attempt's live signals and adjudicates any that trip
// a trigger (silence, spin, cliff), plus the one-time proactive soft-budget
// nudge. A kill respects enforce_kill: shadow mode (the default) journals
// attempt.would_reap with the full decision and cancels nothing; enforce mode
// cancels through the existing target-cancel path and journals attempt.reaped.
// Grants and continues act regardless of enforce_kill (they are safe).

import (
	"context"
	"fmt"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/store"
)

// RunSupervision is the sibling sweep to RunAttention: every interval it sweeps
// the running attempts, adjudicating any that trip a trigger (silence, spin,
// cliff) and delivering the one-time proactive soft-budget nudge. It returns
// when ctx is done. A disabled seam never runs.
func (s *Engine) RunSupervision(ctx context.Context, interval time.Duration) {
	if !s.supervisionCfg.EnabledOn() {
		s.log.InfoContext(ctx, "supervision watchdog disabled")
		return
	}
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			s.sweepSupervision(ctx)
		}
	}
}

// sweepSupervision is one watchdog tick. Per-attempt failures are logged and
// left for the next tick.
func (s *Engine) sweepSupervision(ctx context.Context) {
	attempts, err := s.store.RunningAttempts(ctx)
	if err != nil {
		s.log.ErrorContext(ctx, "supervision: list running attempts", "error", err)
		return
	}
	for _, ra := range attempts {
		ev, err := s.assembleEvidence(ctx, ra.ID)
		if err != nil {
			s.log.ErrorContext(ctx, "supervision: assemble evidence", "attempt_id", ra.ID, "error", err)
			continue
		}
		s.maybeNudge(ctx, ra.ID, ev)
		// A prior kill decision already stands (shadow mode leaves the attempt
		// running); don't re-adjudicate it every tick.
		if ev.PriorKill {
			continue
		}
		trigger := detectTrigger(s.supervisionCfg, ev)
		if trigger == "" {
			continue
		}
		v, err := s.recordDecision(ctx, ev, nil, trigger)
		if err != nil {
			s.log.ErrorContext(ctx, "supervision: record decision", "attempt_id", ra.ID, "error", err)
			continue
		}
		if v.Action == store.BudgetKill {
			s.actuateKill(ctx, ra, ev, v, trigger)
		}
	}
}

// detectTrigger reports whether an attempt warrants adjudication this tick, and
// which signal fired. It only gates; classifyBudget makes the actual decision.
func detectTrigger(cfg config.SupervisionConfig, ev supervisionEvidence) string {
	switch {
	case ev.Silent:
		return "silence"
	case ev.WindowSamples >= spinMinSamples && ev.Dominance >= spinDominanceThreshold && !ev.GrewSinceLast:
		return "spin"
	case cfg.HardCeilingTurns > 0 && ev.RunningTurns >= cfg.HardCeilingTurns-cliffMargin:
		return "cliff"
	default:
		return ""
	}
}

// maybeNudge delivers the one-time proactive nudge once an attempt crosses
// ~80% of its soft turn budget: a plain steer telling the agent to request more
// budget or wrap up. Idempotent via the attempt.budget_nudged journal marker.
func (s *Engine) maybeNudge(ctx context.Context, attemptID string, ev supervisionEvidence) {
	if s.supervisionCfg.SoftTurns <= 0 {
		return
	}
	threshold := int(nudgeSoftFraction * float64(s.supervisionCfg.SoftTurns))
	if ev.RunningTurns < threshold {
		return
	}
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		done, err := tx.HasJournal(ctx, store.EntityAttempt, attemptID, "attempt.budget_nudged")
		if err != nil || done {
			return err
		}
		text := fmt.Sprintf("You are at %d of your ~%d soft turn budget. If you need more, call forge_request_budget with a concrete reason; otherwise start wrapping up.", ev.RunningTurns, s.supervisionCfg.SoftTurns)
		if err := tx.EnqueueSteer(ctx, attemptID, text); err != nil {
			return err
		}
		return tx.Journal(ctx, "attempt.budget_nudged", store.EntityAttempt, attemptID, map[string]any{"running_turns": ev.RunningTurns, "soft_turns": s.supervisionCfg.SoftTurns})
	})
	if err != nil {
		s.log.ErrorContext(ctx, "supervision: proactive nudge", "attempt_id", attemptID, "error", err)
	}
}

// actuateKill applies a kill verdict under the enforce_kill gate. Shadow mode
// (the default) journals attempt.would_reap with the full decision and cancels
// nothing — the whole point is to observe the policy before trusting it. Enforce
// mode cancels the attempt through the existing target-cancel path (the worker
// stops on its next heartbeat and the target requeues under its retry policy)
// and journals attempt.reaped.
func (s *Engine) actuateKill(ctx context.Context, ra store.RunningAttempt, ev supervisionEvidence, v budgetVerdict, trigger string) {
	payload := s.evidencePayload(ev, v, trigger)
	if !s.supervisionCfg.EnforceKill {
		if err := s.store.Write(ctx, func(tx *store.Tx) error {
			return tx.Journal(ctx, "attempt.would_reap", store.EntityAttempt, ra.ID, payload)
		}); err != nil {
			s.log.ErrorContext(ctx, "supervision: journal would_reap", "attempt_id", ra.ID, "error", err)
			return
		}
		s.log.InfoContext(ctx, "supervision would reap (shadow mode)", "attempt_id", ra.ID, "trigger", trigger, "rationale", v.Rationale)
		return
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error {
		ok, err := tx.RequestTargetCancel(ctx, ra.TargetID, supervisionActor)
		if err != nil {
			return err
		}
		payload["cancelled"] = ok
		return tx.Journal(ctx, "attempt.reaped", store.EntityAttempt, ra.ID, payload)
	}); err != nil {
		s.log.ErrorContext(ctx, "supervision: reap attempt", "attempt_id", ra.ID, "error", err)
		return
	}
	s.log.WarnContext(ctx, "supervision reaped attempt", "attempt_id", ra.ID, "target_id", ra.TargetID, "trigger", trigger, "rationale", v.Rationale)
}
