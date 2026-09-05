package web

import (
	"context"
	"encoding/json"
	"fmt"
	"os/exec"
	"strconv"
	"strings"

	"forge/internal/core/config"
	"forge/internal/core/directives"
	"forge/internal/core/model"
	"forge/internal/core/store"
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
	// RestoreSkipped: the regression was real but a restore was impossible —
	// a pre-restructure snapshot that no longer maps to a row, or a library
	// git revert that conflicts with later edits; the proposal is closed
	// without one.
	RestoreSkipped bool `json:"restore_skipped,omitempty"`
	// Library-edit reverts (checkABRevertDirective): the commit the proposal
	// applied and the revert commit that undid it.
	AppliedCommit string `json:"applied_commit,omitempty"`
	RevertCommit  string `json:"revert_commit,omitempty"`
}

// checkABReverts is the A/B rule of DESIGN.md §12, run every sweep tick: for
// each applied routine/process proposal with K runs on its new generation,
// compare verified success rate and cost per verified success against the
// previous generation's last K runs; a regression beyond the margin restores
// the previous generation and marks the proposal reverted. This is not Forge
// applying a change of its own: the human's approval covered the A/B plan
// including this revert, and the revert only ever restores a snapshot that
// already existed in routine_generations.
func (s *Engine) checkABReverts(ctx context.Context, cfg config.ReflectionConfig) {
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
		if ref, ok := strings.CutPrefix(p.AppliedRef, "directive:"); ok {
			// A library edit: the ref carries name@commit (apply.go). Older
			// refs without the commit predate the attribution key and cannot
			// be compared — skipped, not failed.
			name, sha, ok := strings.Cut(ref, "@")
			if !ok || sha == "" {
				continue
			}
			if err := s.checkABRevertDirective(ctx, p, name, sha, cfg); err != nil {
				s.log.WarnContext(ctx, "ab check (directive)", "proposal_id", p.ID, "directive", name, "error", err)
			}
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

// checkABRevertDirective is the A/B rule for library edits: runs composed at
// or after the applied commit (its descendants in the library's git DAG)
// against the last runs before it. A regression git-reverts exactly that
// commit; a revert conflict closes the proposal without a restore rather
// than wedging the sweep.
func (s *Engine) checkABRevertDirective(ctx context.Context, p *store.Proposal, name, sha string, cfg config.ReflectionConfig) error {
	lib := s.libraryNow()
	if lib == nil {
		return nil
	}
	facts, err := s.store.FactsByDirective(ctx, name, 4*cfg.K)
	if err != nil {
		return err
	}
	isNew := map[string]bool{}
	classify := func(commit string) bool {
		if v, ok := isNew[commit]; ok {
			return v
		}
		// The edit is "in" a run when the applied commit is an ancestor of
		// (or equals) the commit the run's prompt was composed at.
		v := commit == sha || exec.CommandContext(ctx, "git", "-C", lib.Dir, "merge-base", "--is-ancestor", sha, commit).Run() == nil
		isNew[commit] = v
		return v
	}
	var newFacts, prevFacts []store.AttemptFacts
	for _, f := range facts { // newest first
		if classify(f.LibraryCommit) {
			if len(newFacts) < cfg.K {
				newFacts = append(newFacts, f)
			}
		} else if len(prevFacts) < cfg.K {
			prevFacts = append(prevFacts, f)
		}
	}
	if len(newFacts) < cfg.K || len(prevFacts) == 0 {
		return nil // not enough runs on the edit yet, or nothing to compare
	}
	newRate, newCost := verifiedOutcome(newFacts)
	prevRate, prevCost := verifiedOutcome(prevFacts)
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
	outcome := abOutcome{K: cfg.K, NewRate: newRate, PrevRate: prevRate,
		NewCostPerSuccess: newCost, PrevCostPerSuccess: prevCost,
		RegressedOn: regressedOn, AppliedCommit: sha}
	if err := exec.CommandContext(ctx, "git", "-C", lib.Dir, "-c", "user.name=forge", "-c", "user.email=forge@localhost", "revert", "--no-edit", sha).Run(); err != nil {
		if aerr := exec.CommandContext(ctx, "git", "-C", lib.Dir, "revert", "--abort").Run(); aerr != nil {
			s.log.WarnContext(ctx, "ab revert: abort after conflict", "error", aerr)
		}
		outcome.RestoreSkipped = true
		s.log.WarnContext(ctx, "ab revert: git revert conflicts (later edits touch the same lines); closing without a restore", "directive", name, "commit", sha)
	} else {
		outcome.RevertCommit = directives.Head(lib.Dir)
		if s.promptsReload != nil {
			if rerr := s.promptsReload(); rerr != nil {
				s.log.WarnContext(ctx, "ab revert: reload", "error", rerr)
			}
		}
	}
	b, err := json.Marshal(outcome)
	if err != nil {
		return fmt.Errorf("encode outcome: %w", err)
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		_, werr := tx.MarkProposalReverted(ctx, p.ID, b)
		return werr
	})
	if err != nil {
		return err
	}
	s.log.InfoContext(ctx, "directive proposal auto-reverted", "proposal_id", p.ID, "directive", name,
		"regressed_on", regressedOn, "applied_commit", sha, "revert_commit", outcome.RevertCommit, "restore_skipped", outcome.RestoreSkipped)
	return nil
}

// checkABRevert decides and, on regression, performs one proposal's revert.
func (s *Engine) checkABRevert(ctx context.Context, p *store.Proposal, name string, gen int, cfg config.ReflectionConfig) error {
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
		// identity stays the current row's, so history remains linear. A
		// pre-restructure snapshot (content fields, no target) cannot restore
		// as a row anymore — its content lives in git; skip with a journal
		// entry instead of failing the sweep.
		restored.ID, restored.Name = r.ID, r.Name
		outcome = abOutcome{K: cfg.K, NewRate: newRate, PrevRate: prevRate,
			NewCostPerSuccess: newCost, PrevCostPerSuccess: prevCost,
			RegressedOn: regressedOn, RestoredGeneration: gen - 1}
		if restored.Target == "" {
			// A pre-restructure snapshot cannot restore as a row; close the
			// proposal as reverted-without-restore so the sweep moves on.
			outcome.RestoreSkipped = true
			s.log.WarnContext(ctx, "ab revert: pre-restructure snapshot cannot restore (content lives in the directive); closing without a restore", "routine", r.Name, "generation", gen-1)
		} else {
			if err := tx.UpdateRoutineFrom(ctx, &restored, r.Generation, "proposal:"+p.ID+":revert"); err != nil {
				return err
			}
			outcome.NewGeneration = restored.Generation
		}
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
