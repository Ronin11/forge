package web

import (
	"context"
	"os"
	"path/filepath"
	"sort"
	"strings"
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

// cancelDeadDependants is the sweep pass for the wedge class the conflict
// recovery does not cover: a work blocked on:success of a creator that
// terminated WITHOUT success (unverified, failed, cancelled) can never
// release — the settlement cascade handles failed batch siblings, but a
// non-success CREATOR left eight works parked at the head of the queue for
// 19 hours (director proposal 7c56089b). Cancelling cascades naturally: each
// cancellation is itself a non-success terminal the next tick sees.
func (s *Engine) cancelDeadDependants(ctx context.Context) {
	dead, err := s.store.DeadSuccessDependants(ctx)
	if err != nil {
		s.log.ErrorContext(ctx, "dead dependants: list", "error", err)
		return
	}
	for _, d := range dead {
		err := s.store.Write(ctx, func(tx *store.Tx) error {
			if err := tx.CancelWork(ctx, d.WorkID, "dead-dependency"); err != nil {
				return err
			}
			return tx.Journal(ctx, "work.dep_dead_cancelled", store.EntityWork, d.WorkID, map[string]any{
				"blocked_by": d.BlockedBy, "blocker_state": d.BlockerState, "reason": "on:success dependency terminated without success"})
		})
		if err != nil {
			s.log.WarnContext(ctx, "dead dependants: cancel", "work_id", d.WorkID, "error", err)
			continue
		}
		s.log.InfoContext(ctx, "dead dependant cancelled", "work_id", d.WorkID, "blocked_by", d.BlockedBy, "blocker_state", d.BlockerState)
	}
}

// benchReapAge is how long a settled benchmark's throwaway repo survives.
const benchReapAge = 7 * 24 * time.Hour

// reapBenchRepos is the daemon half of the disk janitor: a bench-* repository
// whose whole tree settled past the window is archived and its directory
// deleted — the scores, facts, and learning live in the store; the checkout
// is scaffolding. Also trims pre-migration DB snapshots to the newest three.
func (s *Engine) reapBenchRepos(ctx context.Context) {
	repos, err := s.store.Repositories(ctx)
	if err != nil {
		s.log.ErrorContext(ctx, "bench reaper: repos", "error", err)
		return
	}
	cutoff := s.now().Add(-benchReapAge)
	for _, r := range repos {
		if r.Archived || !strings.HasPrefix(r.Name, "bench-") {
			continue
		}
		open, newest, err := s.store.RepoWorkAges(ctx, r.Name)
		if err != nil || open > 0 || newest.IsZero() || newest.After(cutoff) {
			continue
		}
		if s.archiveRepo != nil {
			if _, err := s.archiveRepo(ctx, r.Name, r.Path); err != nil {
				s.log.WarnContext(ctx, "bench reaper: archive", "repository", r.Name, "error", err)
				continue
			}
		}
		if strings.Contains(r.Path, "bench-") { // belt and suspenders before rm -rf
			if err := os.RemoveAll(r.Path); err != nil {
				s.log.WarnContext(ctx, "bench reaper: remove dir", "path", r.Path, "error", err)
			}
		}
		if err := s.store.Write(ctx, func(tx *store.Tx) error {
			return tx.Journal(ctx, "repo.bench_reaped", store.EntityDaemon, r.Name, map[string]any{"path": r.Path, "settled": newest})
		}); err == nil {
			s.log.InfoContext(ctx, "bench repo reaped", "repository", r.Name, "path", r.Path)
		}
	}
	s.trimDBSnapshots()
}

// trimDBSnapshots keeps the newest three pre-migration database snapshots.
func (s *Engine) trimDBSnapshots() {
	snaps, err := filepath.Glob(filepath.Join(s.home, "forge.sqlite3.pre-*"))
	if err != nil || len(snaps) <= 3 {
		return
	}
	sort.Strings(snaps) // ULID-named: lexical order is chronological
	for _, old := range snaps[:len(snaps)-3] {
		if err := os.Remove(old); err == nil {
			s.log.InfoContext(context.Background(), "pre-migration snapshot trimmed", "path", old)
		}
	}
}
