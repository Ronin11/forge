package controlplane

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
