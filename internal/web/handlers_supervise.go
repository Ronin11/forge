package web

// The return path of recursive decomposition (DESIGN.md §20): every plan
// batch gets a continuation Work in supervise mode, blocked on:terminal on
// all its members, that reviews what actually landed and either declares the
// goal met (with 1-5 scores) or emits corrective tasks — the next round,
// with a new continuation, bounded by [plan] supervise_rounds. Nested plans
// (planTask mode "plan") recurse through the same machinery, bounded by
// [plan] max_nesting.
//
// Release ordering is the re-block rule: whenever a continuation C is
// created for creator P, every open Work blocked on P is additionally
// blocked on C (on:terminal) — so nothing upstream releases before the
// subtree settles, transitively across rounds and nesting.
//
// Accepted staleness: retrying or reverifying a batch task AFTER its
// continuation ran does not re-trigger supervision — the review judged a
// moment in time, and the human who reopened the task owns the consequence.
// A failed continuation attempt stops the rounds (ancestors release
// on:terminal); a human retry of its target re-enters superviseFollowUps
// normally.

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

// Continuation bounds and defaults. planAncestryBound stops a corrupt
// caused_by chain from looping, mirroring spawnAncestryBound.
const (
	superviseMaxTurns      = 40
	superviseTimeout       = 1800
	defaultSuperviseRounds = 3
	defaultMaxPlanNesting  = 2
	planAncestryBound      = 32
)

// planLimits resolves the [plan] config with its defaults (zero-config test
// servers included).
func (s *Engine) planLimits() (supervise bool, rounds, nesting int) {
	supervise = s.planCfg.Supervise == nil || *s.planCfg.Supervise
	rounds, nesting = s.planCfg.SuperviseRounds, s.planCfg.MaxNesting
	if rounds <= 0 {
		rounds = defaultSuperviseRounds
	}
	if nesting <= 0 {
		nesting = defaultMaxPlanNesting
	}
	return supervise, rounds, nesting
}

// createContinuation creates the supervise Work that reviews the batch
// creator just produced, blocks it on every batch member (on:terminal), and
// re-blocks creator's open dependants onto it so nothing upstream releases
// before the subtree settles. The goal rides in the snapshot's Objective so
// later rounds carry it verbatim.
func (s *Engine) createContinuation(ctx context.Context, tx *store.Tx, creator *store.Work, t *store.Target, snap store.Routine, batchIDs, titles []string, round int, goal, summary string, integrate bool) (*store.Work, error) {
	_, rounds, _ := s.planLimits()
	var b strings.Builder
	fmt.Fprintf(&b, "You supervise the task batch created for this goal:\n\n%s\n\n", goal)
	if summary != "" {
		fmt.Fprintf(&b, "The batch author's summary: %s\n\n", summary)
	}
	b.WriteString("Tasks (your prompt was written before they ran — call forge_work_outcomes first for what actually happened):\n")
	for i, id := range batchIDs {
		fmt.Fprintf(&b, "- %s: %s\n", model.ShortID(id), titles[i])
	}
	fmt.Fprintf(&b, "\nRound %d of %d.", round, rounds)
	if round >= rounds {
		b.WriteString(" This is the final round: you may not emit tasks — report done with honest scores and put the remaining gaps in weakness.")
	}
	prompt := b.String()
	rt := store.Routine{
		Name: "supervise", Mode: "supervise", Prompt: prompt, Objective: goal, Repositories: []string{t.Repository},
		Executor: snap.Executor, Model: snap.Model, MaxTurns: superviseMaxTurns, TimeoutSeconds: superviseTimeout,
		Autonomy: creator.Autonomy, Priority: creator.Priority, BudgetClass: creator.BudgetClass, Concurrency: 1,
		Integrate: integrate, MaxQuestions: adHocMaxQuestions, RequireSandbox: snap.RequireSandbox,
	}
	blob, err := json.Marshal(rt)
	if err != nil {
		return nil, fmt.Errorf("snapshot continuation: %w", err)
	}
	work := &store.Work{
		RoutineName: "supervise", Title: "supervise: " + creator.Title, Trigger: model.TriggerDependency,
		Snapshot: blob, Priority: creator.Priority, BudgetClass: creator.BudgetClass, Autonomy: creator.Autonomy,
		Integrate: false, Size: creator.Size, PlanBatchID: creator.ID, PromptHash: promptHashOf(prompt),
		SubmittedBy:    "plan:" + model.ShortID(creator.ID),
		CausedByWorkID: creator.ID, Cause: model.CauseContinuation,
	}
	if _, err := tx.CreateWork(ctx, work, []string{t.Repository}, nil); err != nil {
		return nil, fmt.Errorf("create continuation: %w", err)
	}
	for _, id := range batchIDs {
		if err := tx.AddDependency(ctx, model.Edge{Work: work.ID, BlockedBy: id, On: model.OnTerminal}); err != nil {
			return nil, fmt.Errorf("continuation edge on %s: %w", id, err)
		}
	}
	// The re-block rule: chain creator's open dependants onto the new
	// continuation, so a work waiting on a decomposed task waits for the
	// whole subtree. A human-wired edge that would cycle is skipped, never
	// fatal — this runs inside a child's completion transaction.
	dependants, err := tx.OpenDependants(ctx, creator.ID)
	if err != nil {
		return nil, err
	}
	for _, x := range dependants {
		if x == work.ID {
			continue
		}
		err := tx.AddDependency(ctx, model.Edge{Work: x, BlockedBy: work.ID, On: model.OnTerminal})
		if errors.Is(err, store.ErrConflict) {
			if jerr := tx.Journal(ctx, "plan.reblock_skipped", store.EntityWork, x, map[string]any{"continuation": work.ID, "reason": "cycle"}); jerr != nil {
				return nil, jerr
			}
			continue
		}
		if err != nil {
			return nil, err
		}
	}
	if err := tx.Journal(ctx, "plan.continuation_created", store.EntityWork, work.ID, map[string]any{
		"batch": creator.ID, "round": round, "tasks": len(batchIDs), "reblocked": len(dependants),
	}); err != nil {
		return nil, err
	}
	s.log.InfoContext(ctx, "continuation created", "work_id", work.ID, "batch", creator.ID, "round", round)
	return work, nil
}

// superviseAssessment is the assessment object of a supervise result.
type superviseAssessment struct {
	Outcome  string         `json:"outcome"`
	Scores   map[string]int `json:"scores"`
	Weakness string         `json:"weakness"`
}

// superviseFollowUps handles a successful supervise completion: done ends
// the subtree (everything re-blocked on this continuation releases when it
// goes terminal), revise fans out the next round under the round cap.
func (s *Engine) superviseFollowUps(ctx context.Context, tx *store.Tx, a *store.Attempt, w *store.Work, t *store.Target, env *protocol.ResultEnvelope) error {
	var assess superviseAssessment
	if env == nil || env.Extra["assessment"] == nil || json.Unmarshal(env.Extra["assessment"], &assess) != nil {
		return tx.Journal(ctx, "supervise.malformed", store.EntityWork, w.ID, map[string]any{"attempt_id": a.ID})
	}
	round, err := s.continuationRound(ctx, tx, w)
	if err != nil {
		return err
	}
	if err := tx.Journal(ctx, "supervise.assessment", store.EntityWork, w.ID, map[string]any{
		"outcome": assess.Outcome, "scores": assess.Scores, "weakness": assess.Weakness, "round": round, "attempt_id": a.ID,
	}); err != nil {
		return err
	}
	if assess.Outcome != "revise" {
		return tx.Journal(ctx, "supervise.done", store.EntityWork, w.ID, map[string]any{"round": round})
	}
	_, rounds, _ := s.planLimits()
	if round >= rounds {
		s.log.WarnContext(ctx, "supervision rounds exhausted", "work_id", w.ID, "round", round, "weakness", assess.Weakness)
		return tx.Journal(ctx, "supervise.rounds_exhausted", store.EntityWork, w.ID, map[string]any{"round": round, "weakness": assess.Weakness})
	}
	tasks, err := planTasks(env)
	if err != nil {
		return fmt.Errorf("supervise %s: %w", a.ID, err)
	}
	if len(tasks) == 0 {
		return tx.Journal(ctx, "supervise.revise_without_tasks", store.EntityWork, w.ID, nil)
	}
	if has, err := tx.HasBatch(ctx, w.ID); err != nil {
		return err
	} else if has {
		return tx.Journal(ctx, "plan.batch_exists", store.EntityWork, w.ID, map[string]any{"attempt_id": a.ID})
	}
	snap, err := snapshotRoutine(*w)
	if err != nil {
		return err
	}
	ids, titles, err := s.createTaskBatch(ctx, tx, w, t.Repository, tasks, snap, snap.Integrate)
	if err != nil {
		return err
	}
	if err := tx.Journal(ctx, "plan.batch_created", store.EntityWork, w.ID, map[string]any{"tasks": ids, "repository": t.Repository, "attempt_id": a.ID, "round": round + 1}); err != nil {
		return err
	}
	_, err = s.createContinuation(ctx, tx, w, t, snap, ids, titles, round+1, snap.Objective, env.Summary, snap.Integrate)
	return err
}

// continuationRound counts consecutive cause=continuation links ending at w
// (w included): unforgeable, no column needed — the tool-spawn depth-walk
// precedent.
func (s *Engine) continuationRound(ctx context.Context, tx *store.Tx, w *store.Work) (int, error) {
	round := 0
	cur := w
	for hop := 0; hop < planAncestryBound && cur != nil && cur.Cause == model.CauseContinuation; hop++ {
		round++
		if cur.CausedByWorkID == "" {
			break
		}
		parent, err := tx.GetWork(ctx, cur.CausedByWorkID)
		if err != nil {
			return 0, err
		}
		cur = parent
	}
	return round, nil
}

// planDepth counts plan-mode Works on creator's caused_by chain, creator
// included — the nesting gate's input. Continuations interleave with plans,
// so the whole (bounded) chain is walked, not just consecutive links.
func (s *Engine) planDepth(ctx context.Context, tx *store.Tx, creator *store.Work) (int, error) {
	depth := 0
	cur := creator
	for hop := 0; hop < planAncestryBound && cur != nil; hop++ {
		if snap, err := snapshotRoutine(*cur); err == nil && snap.Mode == "plan" {
			depth++
		}
		if cur.CausedByWorkID == "" {
			break
		}
		parent, err := tx.GetWork(ctx, cur.CausedByWorkID)
		if err != nil {
			return 0, err
		}
		cur = parent
	}
	return depth, nil
}

// settlePlanBatch cancels open members of a plan batch whose dependencies
// failed, so the batch reaches terminal settlement and its continuation —
// the repair path — can run. Without it, a task blocked on a failed sibling
// stays open forever as dependency_failed and the continuation never fires.
// Iterates to a fixpoint: a cancellation can fail the next sibling's dep.
// Continuations are immune by construction (only on:terminal edges).
func (s *Engine) settlePlanBatch(ctx context.Context, tx *store.Tx, batchID string) error {
	for pass := 0; pass < 25; pass++ {
		open, err := tx.OpenBatchWorks(ctx, batchID)
		if err != nil {
			return err
		}
		if len(open) == 0 {
			return nil
		}
		ids := make([]string, len(open))
		for i, w := range open {
			ids[i] = w.ID
		}
		edges, err := tx.EdgesForWorks(ctx, ids)
		if err != nil {
			return err
		}
		states := map[string]model.WorkState{}
		for _, e := range edges {
			if _, done := states[e.BlockedBy]; done {
				continue
			}
			st, err := s.workStateInTx(ctx, tx, e.BlockedBy)
			if err != nil {
				return err
			}
			states[e.BlockedBy] = st
		}
		edgesByWork := map[string][]model.Edge{}
		for _, e := range edges {
			edgesByWork[e.Work] = append(edgesByWork[e.Work], e)
		}
		cancelled := 0
		for _, w := range open {
			deps := model.Dependencies(edgesByWork[w.ID], states)
			if len(deps.FailedDeps) == 0 {
				continue
			}
			if err := tx.CancelWork(ctx, w.ID, "daemon"); err != nil {
				return err
			}
			if err := tx.Journal(ctx, "plan.task_dep_cancelled", store.EntityWork, w.ID, map[string]any{"batch": batchID, "failed_on": deps.FailedDeps}); err != nil {
				return err
			}
			cancelled++
		}
		if cancelled == 0 {
			return nil
		}
	}
	return fmt.Errorf("settle batch %s: no fixpoint after 25 passes", batchID)
}

// workStateInTx derives one Work's state from its targets, reading through
// the transaction so a state changed earlier in the same tx is seen. A
// missing work reports as failed (model.Dependencies' rule for missing
// states holds either way).
func (s *Engine) workStateInTx(ctx context.Context, tx *store.Tx, workID string) (model.WorkState, error) {
	w, err := tx.GetWork(ctx, workID)
	if errors.Is(err, store.ErrNotFound) {
		return model.WorkFailed, nil
	}
	if err != nil {
		return "", err
	}
	ts, err := tx.TargetsForWork(ctx, w.ID)
	if err != nil {
		return "", err
	}
	states := make([]model.State, len(ts))
	for i, t := range ts {
		states[i] = t.State
	}
	return model.DeriveWorkState(model.WorkInputs{Targets: states, Integrate: w.Integrate}), nil
}
