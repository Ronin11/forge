package controlplane

import (
	"context"
	"encoding/json"
	"fmt"
	"strconv"
	"strings"

	"forge/internal/core/model"
	"forge/internal/store"
)

// abOutcome is what MarkProposalReverted records as outcome_metrics: the
// numbers that triggered the revert (DESIGN.md §12).
type abOutcome struct {
	K                  int      `json:"k"`
	NewRate            float64  `json:"new_rate"`
	PrevRate           float64  `json:"prev_rate"`
	NewCostPerSuccess  *float64 `json:"new_cost_per_success"`
	PrevCostPerSuccess *float64 `json:"prev_cost_per_success"`
	RegressedOn        string   `json:"regressed_on"` // "rate" | "cost"
	RestoredGeneration int      `json:"restored_generation"`
	NewGeneration      int      `json:"new_generation"`
}

// checkABReverts is the A/B rule of DESIGN.md §12, run every sweep tick: for
// each applied routine/process proposal with K runs on its new generation,
// compare verified success rate and cost per verified success against the
// previous generation's last K runs; a regression beyond the margin restores
// the previous generation and marks the proposal reverted. This is not Forge
// applying a change of its own: the human's approval covered the A/B plan
// including this revert, and the revert only ever restores a snapshot that
// already existed in routine_generations.
func (s *Server) checkABReverts(ctx context.Context, cfg ReflectionConfig) {
	applied, err := s.store.ListProposals(ctx, model.ProposalApplied)
	if err != nil {
		s.log.ErrorContext(ctx, "ab: list applied proposals", "error", err)
		return
	}
	for i := range applied {
		p := &applied[i]
		if p.Kind != model.ProposalRoutine && p.Kind != model.ProposalProcess {
			continue
		}
		genStr, ok := strings.CutPrefix(p.AppliedRef, "generation:")
		if !ok {
			continue
		}
		gen, err := strconv.Atoi(genStr)
		if err != nil || gen < 2 {
			continue
		}
		name, ok := strings.CutPrefix(p.Target, "routine:")
		if !ok {
			continue
		}
		if err := s.checkABRevert(ctx, p, name, gen, cfg); err != nil {
			s.log.WarnContext(ctx, "ab check", "proposal_id", p.ID, "routine", name, "error", err)
		}
	}
}

// checkABRevert decides and, on regression, performs one proposal's revert.
func (s *Server) checkABRevert(ctx context.Context, p *store.Proposal, name string, gen int, cfg ReflectionConfig) error {
	newFacts, err := s.store.FactsByRoutineGeneration(ctx, name, gen, cfg.K)
	if err != nil {
		return fmt.Errorf("facts of generation %d: %w", gen, err)
	}
	if len(newFacts) < cfg.K {
		return nil // not enough runs on the new generation yet
	}
	prevFacts, err := s.store.FactsByRoutineGeneration(ctx, name, gen-1, cfg.K)
	if err != nil {
		return fmt.Errorf("facts of generation %d: %w", gen-1, err)
	}
	if len(prevFacts) == 0 {
		return nil // nothing to compare against
	}
	newRate, newCost := verifiedOutcome(newFacts)
	prevRate, prevCost := verifiedOutcome(prevFacts)
	// A generation with zero verified successes has rate 0 (always a rate
	// regression when the previous rate was positive) and no computable cost.
	regressedOn := ""
	switch {
	case prevRate > 0 && newRate < prevRate*(1-cfg.Margin):
		regressedOn = "rate"
	case newCost != nil && prevCost != nil && *newCost > *prevCost*(1+cfg.Margin):
		regressedOn = "cost"
	}
	if regressedOn == "" {
		return nil
	}
	var outcome abOutcome
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		r, err := tx.GetRoutine(ctx, name)
		if err != nil {
			return err
		}
		snap, err := tx.GenerationSnapshot(ctx, r.ID, gen-1)
		if err != nil {
			return err
		}
		var restored store.Routine
		if err := json.Unmarshal(snap, &restored); err != nil {
			return fmt.Errorf("decode snapshot of generation %d: %w", gen-1, err)
		}
		// The restore is a NEW generation whose content equals the snapshot;
		// identity stays the current row's, so history remains linear.
		restored.ID, restored.Name = r.ID, r.Name
		if err := tx.UpdateRoutineFrom(ctx, &restored, r.Generation, "proposal:"+p.ID+":revert"); err != nil {
			return err
		}
		outcome = abOutcome{K: cfg.K, NewRate: newRate, PrevRate: prevRate,
			NewCostPerSuccess: newCost, PrevCostPerSuccess: prevCost,
			RegressedOn: regressedOn, RestoredGeneration: gen - 1, NewGeneration: restored.Generation}
		b, err := json.Marshal(outcome)
		if err != nil {
			return fmt.Errorf("encode outcome: %w", err)
		}
		_, err = tx.MarkProposalReverted(ctx, p.ID, b)
		return err
	})
	if err != nil {
		return err
	}
	s.log.InfoContext(ctx, "proposal auto-reverted", "proposal_id", p.ID, "routine", name,
		"regressed_on", outcome.RegressedOn, "restored_generation", outcome.RestoredGeneration,
		"new_generation", outcome.NewGeneration)
	return nil
}

// verifiedOutcome computes the A/B rule's two measures over one generation's
// facts: verified success rate (model.IsSuccess and verification passed, over
// all runs) and cost per verified success (nil when there were none).
func verifiedOutcome(facts []store.AttemptFacts) (rate float64, costPerSuccess *float64) {
	var successes int
	var cost float64
	for _, f := range facts {
		if f.CostUSD != nil {
			cost += *f.CostUSD
		}
		if model.IsSuccess(f.State) && f.VerificationPass != nil && *f.VerificationPass {
			successes++
		}
	}
	rate = float64(successes) / float64(len(facts))
	if successes > 0 {
		c := cost / float64(successes)
		costPerSuccess = &c
	}
	return rate, costPerSuccess
}
