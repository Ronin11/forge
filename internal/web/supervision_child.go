package web

// Phase A runtime — the child-initiated path behind forge_request_budget and
// the shared record step. recordDecision adjudicates one request, appends it to
// the extension ledger, and journals the audited outcome (attempt.budget_requested
// plus attempt.budget_granted or attempt.budget_denied); a grant rides the
// journal to the worker on the next heartbeat. The watchdog (Phase B) reuses
// recordDecision and evidencePayload for its own triggers.

import (
	"context"
	"encoding/json"
	"fmt"

	"forge/internal/core/store"
)

// recordDecision adjudicates one request, appends it to the extension ledger,
// and journals the audited outcome, all in one transaction. A grant is queued
// for the worker on the journal (attempt.budget_granted, taken on the next
// heartbeat); a denial of a child ask is journaled attempt.budget_denied. A
// kill is recorded on the ledger but actuated by the caller (which owns the
// enforce_kill gate). The verdict is returned for the caller to act on.
func (s *Engine) recordDecision(ctx context.Context, ev supervisionEvidence, ask *budgetAsk, reason string) (budgetVerdict, error) {
	v := s.adjudicate(ctx, ev, ask)
	// At a cliff the executor dies at its cap regardless of the verdict, so
	// "continue" is only actionable as turns: an attempt with evident
	// progress gets a bounded extension instead of dying mid-flight with the
	// supervisor's approval on record (three continue verdicts preceded a
	// budget_cliff death on 2026-09-06). Flailing attempts still get kill.
	if reason == "cliff" && ask == nil && v.Action == store.BudgetContinue && ev.GrewSinceLast &&
		(s.supervisionCfg.MaxAutoExtensions <= 0 || ev.PriorGrants < s.supervisionCfg.MaxAutoExtensions) {
		slot := s.supervisionCfg.SoftTurns
		if slot <= 0 {
			slot = 20
		}
		if amt := boundedGrant(s.supervisionCfg, ev, &budgetAsk{Dimension: store.BudgetTurns, Amount: float64(slot)}); amt > 0 {
			v.Action, v.Dimension, v.GrantedAmount, v.DecidedBy = store.BudgetExtend, store.BudgetTurns, amt, v.DecidedBy
			v.Rationale = "cliff with evident progress — continue converted to extension: " + v.Rationale
		}
	}

	dimension, amount := store.BudgetTurns, 0.0
	if ask != nil {
		dimension, amount = ask.Dimension, ask.Amount
	} else if v.Dimension != "" {
		dimension = v.Dimension
	}
	snapshot, err := json.Marshal(map[string]any{
		"running_turns":   ev.RunningTurns,
		"artifact_growth": ev.ArtifactGrowth,
		"tokens_in":       ev.TokensIn,
		"tokens_out":      ev.TokensOut,
	})
	if err != nil {
		return v, fmt.Errorf("marshal progress snapshot: %w", err)
	}
	nudge := ""
	if v.Action == store.BudgetExtend {
		nudge = fmt.Sprintf("Budget extended: %g more %s granted. %s Keep going.", v.GrantedAmount, v.Dimension, v.Rationale)
	}

	err = s.store.Write(ctx, func(tx *store.Tx) error {
		if _, err := tx.AppendBudgetRequest(ctx, store.BudgetRequest{
			AttemptID: ev.AttemptID, Dimension: dimension, Amount: amount, Reason: reason,
			Decision: v.Action, GrantedAmount: v.GrantedAmount, ProgressSnapshot: snapshot,
			DecidedBy: v.DecidedBy, Rationale: v.Rationale,
		}); err != nil {
			return err
		}
		if err := tx.Journal(ctx, "attempt.budget_requested", store.EntityAttempt, ev.AttemptID, s.evidencePayload(ev, v, reason)); err != nil {
			return err
		}
		switch {
		case v.Action == store.BudgetExtend:
			// Both the audit record and the actuation queue: TakeBudgetGrants
			// hands this to the worker on its next heartbeat.
			return tx.Journal(ctx, "attempt.budget_granted", store.EntityAttempt, ev.AttemptID, map[string]any{
				"dimension": v.Dimension, "granted_amount": v.GrantedAmount, "nudge": nudge,
				"decided_by": v.DecidedBy, "rationale": v.Rationale,
			})
		case ask != nil && v.Action == store.BudgetContinue:
			return tx.Journal(ctx, "attempt.budget_denied", store.EntityAttempt, ev.AttemptID, map[string]any{
				"dimension": dimension, "amount": amount, "decided_by": v.DecidedBy, "rationale": v.Rationale,
			})
		default:
			return nil
		}
	})
	if err != nil {
		return v, err
	}
	s.log.InfoContext(ctx, "supervision decision", "attempt_id", ev.AttemptID, "action", v.Action,
		"granted", v.GrantedAmount, "decided_by", v.DecidedBy, "reason", reason)
	return v, nil
}

// evidencePayload is the journaled evidence + rationale + decided_by for one
// decision — mirrors question.auto_answered's audited shape.
func (s *Engine) evidencePayload(ev supervisionEvidence, v budgetVerdict, reason string) map[string]any {
	return map[string]any{
		"trigger":         reason,
		"action":          v.Action,
		"decided_by":      v.DecidedBy,
		"rationale":       v.Rationale,
		"running_turns":   ev.RunningTurns,
		"soft_turns":      s.supervisionCfg.SoftTurns,
		"hard_ceiling":    s.supervisionCfg.HardCeilingTurns,
		"artifact_growth": ev.ArtifactGrowth,
		"prior_requests":  ev.PriorRequests,
		"prior_grants":    ev.PriorGrants,
		"dominance":       ev.Dominance,
		"window_samples":  ev.WindowSamples,
		"silent":          ev.Silent,
		"silence_seconds": int(ev.SilenceFor.Seconds()),
	}
}

// AdjudicateBudgetRequest is the child-initiated path behind forge_request_budget:
// it assembles evidence, adjudicates the ask, records it, and returns the
// child-facing outcome. A grant is queued for the worker via the heartbeat; a
// kill or continue reads to the child as a denial (the watchdog owns the reap).
func (s *Engine) AdjudicateBudgetRequest(ctx context.Context, attemptID, dimension string, amount float64, reason string) (store.BudgetOutcome, error) {
	if !s.supervisionCfg.EnabledOn() {
		return store.BudgetOutcome{Decision: "denied", Message: "supervision is disabled; proceed within your current budget"}, nil
	}
	ev, err := s.assembleEvidence(ctx, attemptID)
	if err != nil {
		return store.BudgetOutcome{}, err
	}
	if dimension == "model" {
		return s.adjudicateModelEscalation(ctx, ev, reason)
	}
	ask := &budgetAsk{Dimension: dimension, Amount: amount, Reason: reason}
	v, err := s.recordDecision(ctx, ev, ask, reason)
	if err != nil {
		return store.BudgetOutcome{}, err
	}
	if v.Action == store.BudgetExtend {
		return store.BudgetOutcome{Decision: "granted", GrantedAmount: v.GrantedAmount,
			Message: fmt.Sprintf("granted %g more %s: %s", v.GrantedAmount, v.Dimension, v.Rationale)}, nil
	}
	return store.BudgetOutcome{Decision: "denied", Message: "not granted: " + v.Rationale}, nil
}

// adjudicateModelEscalation handles dimension="model": the agent believes the
// task exceeds its model. Self-assessment alone is not trusted — the decider
// weighs it against the objective evidence (turns, artifact growth, spin) —
// but a grant here does not extend the run: it journals escalation.granted on
// the target, tells the agent to finish with a handoff summary and stop, and
// the sweep auto-retries the non-success completion once, which the routing
// ladder then escalates one rung.
func (s *Engine) adjudicateModelEscalation(ctx context.Context, ev supervisionEvidence, reason string) (store.BudgetOutcome, error) {
	if len(s.routing.Ladder) == 0 {
		return store.BudgetOutcome{Decision: "denied", Message: "no escalation ladder is configured; do your best within this model"}, nil
	}
	if ev.RunningTurns < 5 {
		return store.BudgetOutcome{Decision: "denied", Message: "too early to conclude the task exceeds this model — make a real attempt first"}, nil
	}
	att, err := s.store.GetAttempt(ctx, ev.AttemptID)
	if err != nil {
		return store.BudgetOutcome{}, err
	}
	targetID := att.TargetID
	if has, _ := s.hasTargetJournal(ctx, targetID, "escalation.granted"); has {
		return store.BudgetOutcome{Decision: "denied", Message: "escalation was already granted for this target; finish your handoff and stop"}, nil
	}
	ask := &budgetAsk{Dimension: "model", Amount: 1, Reason: "MODEL ESCALATION REQUEST (grant=extend, refuse=continue): " + reason}
	v, err := s.recordDecision(ctx, ev, ask, reason)
	if err != nil {
		return store.BudgetOutcome{}, err
	}
	if v.Action != store.BudgetExtend {
		return store.BudgetOutcome{Decision: "denied", Message: "escalation not granted: " + v.Rationale + " — continue within this model"}, nil
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error {
		return tx.Journal(ctx, "escalation.granted", store.EntityTarget, targetID, map[string]any{
			"attempt_id": ev.AttemptID, "reason": reason, "decided_by": v.DecidedBy})
	}); err != nil {
		return store.BudgetOutcome{}, err
	}
	s.log.InfoContext(ctx, "model escalation granted", "attempt_id", ev.AttemptID, "target_id", targetID, "decided_by", v.DecidedBy)
	return store.BudgetOutcome{Decision: "granted", GrantedAmount: 1,
		Message: "ESCALATION APPROVED. Stop working the task now: write a handoff summary in your result — what you tried, what you ruled out, exactly where you are stuck — then finish. A stronger model resumes from your notes."}, nil
}

// hasTargetJournal is HasJournal through the reader pool (no tx on this path).
func (s *Engine) hasTargetJournal(ctx context.Context, targetID, kind string) (bool, error) {
	var has bool
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		var herr error
		has, herr = tx.HasJournal(ctx, store.EntityTarget, targetID, kind)
		return herr
	})
	return has, err
}
