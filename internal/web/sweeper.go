package web

import (
	"context"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/model"
	"forge/internal/core/store"
)

// RunSweeper is DESIGN.md §14's lease sweeper. On start it grants every live
// lease RestartGrace (a daemon restart never expires running attempts); then,
// every interval, it fails Targets whose lease lapsed and records their facts.
// Each tick also runs the A/B auto-revert check over applied proposals
// (ab.go), with the configured K and margin. It returns when ctx is done.
func (s *Engine) RunSweeper(ctx context.Context, interval time.Duration, reflection config.ReflectionConfig) {
	var extended int
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		n, err := tx.ExtendLeases(ctx)
		extended = n
		return err
	})
	if err != nil {
		s.log.ErrorContext(ctx, "extend leases at start", "error", err)
	} else {
		s.log.InfoContext(ctx, "leases extended", "targets", extended, "grace", store.RestartGrace.String())
	}
	// The assignment cache must exist before the first claim, not a tick
	// later: a work created in the restart-to-first-tick gap ran unenrolled
	// (reflect 5, 2026-09-05) and its experiment lost the sample.
	s.refreshLiveExperiments(ctx)
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	// Auto-eval runs its scoring in goroutines this loop owns; wait for them
	// before returning so no eval outlives the sweeper (STYLE.md §3).
	defer s.autoEvalWG.Wait()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			s.sweep(ctx)
			s.refreshLiveExperiments(ctx)
			s.decideLiveExperiments(ctx)
			s.checkABReverts(ctx, reflection)
			s.resolvePredictions(ctx)
			s.sweepAutoEval(ctx)
		}
	}
}

// sweep is one tick: the expiry transitions and the facts of every Target they
// made terminal, in one transaction, so a swept Target never lacks its row.
// The attempt for each swept Target is looked up through the reader pool; it
// was committed at claim time, long before this transaction. Errors are logged
// here — the next tick retries; nothing else handles them.
func (s *Engine) sweep(ctx context.Context) {
	var swept []store.Target
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		var err error
		swept, err = tx.SweepExpiredLeases(ctx)
		if err != nil {
			return err
		}
		settled := map[string]bool{}
		for _, t := range swept {
			w, err := tx.GetWork(ctx, t.WorkID)
			if err != nil {
				return err
			}
			if !model.IsTerminal(t.State, w.Integrate) {
				continue
			}
			// A swept batch member dies outside the complete handler, so the
			// settlement cascade (handlers_supervise.go) runs here too.
			if w.PlanBatchID != "" && !settled[w.PlanBatchID] {
				settled[w.PlanBatchID] = true
				if err := s.settlePlanBatch(ctx, tx, w.PlanBatchID); err != nil {
					return err
				}
			}
			a, err := s.store.AttemptForTarget(ctx, t.ID)
			if err != nil {
				return err
			}
			if a == nil {
				continue
			}
			if err := s.recordFacts(ctx, tx, a.ID); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		s.log.ErrorContext(ctx, "sweep expired leases", "error", err)
		return
	}
	for _, t := range swept {
		s.log.InfoContext(ctx, "lease expired", "target_id", t.ID, "work_id", t.WorkID, "worker_id", t.WorkerID, "state", t.State)
	}
}
