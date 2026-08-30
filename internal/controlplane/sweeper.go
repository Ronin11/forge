package controlplane

import (
	"context"
	"time"

	"forge/internal/model"
	"forge/internal/store"
)

// RunSweeper is DESIGN.md §14's lease sweeper. On start it grants every live
// lease RestartGrace (a daemon restart never expires running attempts); then,
// every interval, it fails Targets whose lease lapsed and records their facts.
// It returns when ctx is done.
func (s *Server) RunSweeper(ctx context.Context, interval time.Duration) {
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
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			s.sweep(ctx)
		}
	}
}

// sweep is one tick: the expiry transitions and the facts of every Target they
// made terminal, in one transaction, so a swept Target never lacks its row.
// The attempt for each swept Target is looked up through the reader pool; it
// was committed at claim time, long before this transaction. Errors are logged
// here — the next tick retries; nothing else handles them.
func (s *Server) sweep(ctx context.Context) {
	var swept []store.Target
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		var err error
		swept, err = tx.SweepExpiredLeases(ctx)
		if err != nil {
			return err
		}
		for _, t := range swept {
			w, err := tx.GetWork(ctx, t.WorkID)
			if err != nil {
				return err
			}
			if !model.IsTerminal(t.State, w.Integrate) {
				continue
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
