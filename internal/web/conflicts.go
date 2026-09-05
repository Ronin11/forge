package web

import (
	"context"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// Conflict auto-recovery. A merge conflict beyond mergiraf used to park the
// target in the human queue while everything blocked on it waited forever —
// the deadlock wedged three benchmark trees in three days (a supervise
// blocked on:terminal never fires because conflict is not terminal). The
// sweep now recovers unattended: one automatic requeue after a grace (the
// integration branch has usually moved — a fresh rebase often lands), and a
// second conflict cancels the work so the settlement cascade runs and the
// supervise revise round repairs the loss properly.

// conflictGrace is how long a target sits in conflict before the sweep acts.
const conflictGrace = 10 * time.Minute

// conflictRequeueMark is the journal marker distinguishing the first
// recovery (requeue) from the second (cancel).
const conflictRequeueMark = "merge.conflict_autorequeue"

func (s *Engine) recoverConflicts(ctx context.Context) {
	stale, err := s.store.ConflictedTargets(ctx, s.now().Add(-conflictGrace))
	if err != nil {
		s.log.ErrorContext(ctx, "conflict recovery: list", "error", err)
		return
	}
	for _, t := range stale {
		err := s.store.Write(ctx, func(tx *store.Tx) error {
			requeued, err := tx.HasJournal(ctx, store.EntityTarget, t.ID, conflictRequeueMark)
			if err != nil {
				return err
			}
			if !requeued {
				if _, err := tx.Transition(ctx, t.ID, model.QueuedForMerge, store.TransitionOptions{Actor: "conflict-recovery"}); err != nil {
					return err
				}
				if err := tx.ResetMergeAttempts(ctx, t.ID); err != nil {
					return err
				}
				return tx.Journal(ctx, conflictRequeueMark, store.EntityTarget, t.ID, map[string]any{"work_id": t.WorkID})
			}
			// Second conflict: the clash is real (an architectural overlap,
			// not a stale base). Cancel the work — retained worktree and
			// branch survive for the revise round to mine — and settle its
			// batch so the supervise fires now, not never.
			if err := tx.CancelWork(ctx, t.WorkID, "conflict-recovery"); err != nil {
				return err
			}
			w, err := tx.GetWork(ctx, t.WorkID)
			if err != nil {
				return err
			}
			if w.PlanBatchID != "" {
				if err := s.settlePlanBatch(ctx, tx, w.PlanBatchID); err != nil {
					return err
				}
			}
			return tx.Journal(ctx, "merge.conflict_cancelled", store.EntityTarget, t.ID, map[string]any{"work_id": t.WorkID, "reason": "second conflict — settled for the revise round"})
		})
		if err != nil {
			s.log.WarnContext(ctx, "conflict recovery", "target_id", t.ID, "work_id", t.WorkID, "error", err)
			continue
		}
		s.log.InfoContext(ctx, "conflict recovered", "target_id", t.ID, "work_id", t.WorkID)
	}
}
