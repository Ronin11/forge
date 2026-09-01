package worker

import (
	"context"
	"errors"
	"fmt"
	"time"

	"forge/internal/core/logging"
	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"log/slog"
)

// ReconcileReport is what one pass found and did.
type ReconcileReport struct {
	Scanned   int
	Orphans   int // processes killed
	Cleaned   int
	Retained  int
	Untouched int // resumable or awaiting merge, left alone
	Corrupt   int
}

// reconcile applies DESIGN.md §7.3 to every manifest not owned by a running
// attempt: kill orphans, inspect, clean or retain, report to the daemon.
func (w *Worker) reconcile(ctx context.Context) error {
	manifests, corrupt, err := w.runner.manifests.LoadAll()
	if err != nil {
		return err
	}
	rep := ReconcileReport{Scanned: len(manifests), Corrupt: len(corrupt)}
	for _, c := range corrupt {
		w.log.ErrorContext(ctx, "corrupt manifest left in place", "path", c.Path, "error", c.Err)
	}
	var errs error
	for _, m := range manifests {
		if m.Final() {
			continue
		}
		w.mu.Lock()
		_, running := w.active[m.AttemptID]
		w.mu.Unlock()
		if running {
			continue
		}
		if err := w.reconcileOne(ctx, m, &rep); err != nil {
			errs = errors.Join(errs, fmt.Errorf("attempt %s: %w", model.ShortID(m.AttemptID), err))
		}
	}
	w.log.InfoContext(ctx, "reconcile", "scanned", rep.Scanned, "orphans", rep.Orphans, "cleaned", rep.Cleaned, "retained", rep.Retained, "untouched", rep.Untouched, "corrupt", rep.Corrupt)
	return errs
}

func (w *Worker) reconcileOne(ctx context.Context, m *Manifest, rep *ReconcileReport) error {
	ctx = logging.ContextWith(ctx, slog.String("attempt_id", m.AttemptID), slog.String("target_id", m.TargetID), slog.String("work_id", m.WorkID))
	log := w.log
	// 1. Orphaned process.
	if m.ProcessActive && m.PID != 0 {
		alive, err := ProcessAlive(m.PID, m.PIDStart)
		switch {
		case err != nil:
			log.WarnContext(ctx, "cannot verify orphan identity; leaving it", "pid", m.PID, "error", err)
		case alive:
			log.WarnContext(ctx, "killing orphaned agent process", "pid", m.PID)
			if err := KillGroup(m.PID, m.PIDStart, killGrace); err != nil {
				return fmt.Errorf("kill orphan %d: %w", m.PID, err)
			}
			rep.Orphans++
		}
		// The group kill cannot reach a descendant that left the group; the
		// environment markers can (sweep.go). A crashed worker's attempt is
		// exactly the case where those survive unnoticed.
		swept, serr := Sweep(AttemptEnv, m.AttemptID, killGrace)
		if serr != nil {
			log.WarnContext(ctx, "sweep leftover attempt processes", "error", serr)
		}
		if swept > 0 {
			log.WarnContext(ctx, "killed processes the attempt left running", "processes", swept)
			rep.Orphans += swept
		}
		m.ProcessActive = false
		if err := w.runner.manifests.Write(m); err != nil {
			return err
		}
	}
	// 3. What does the daemon believe?
	st, err := w.client.Attempt(ctx, m.AttemptID)
	var se *StatusError
	daemonKnows := err == nil
	if err != nil && !errors.As(err, &se) {
		return fmt.Errorf("daemon unreachable: %w", err)
	}
	if daemonKnows && st.Resumable && !st.Terminal {
		rep.Untouched++
		return nil
	}
	if m.Lifecycle == ManifestAwaitingMerge && daemonKnows && !st.Terminal {
		rep.Untouched++
		return nil
	}
	// 2. Inspect and decide.
	repo, ok := w.runner.repo(m.RepositoryName)
	if !ok {
		m.Lifecycle, m.RetentionReason = ManifestRetained, "repository no longer registered"
		m.CleanupCommand = fmt.Sprintf("forge cleanup %s --confirm", model.ShortID(m.AttemptID))
		rep.Retained++
		w.noteRetention(m)
		return w.runner.manifests.Write(m)
	}
	a := &attempt{r: w.runner, claim: &protocol.Claim{AttemptID: m.AttemptID, TargetID: m.TargetID, WorkID: m.WorkID, Repository: m.RepositoryName, RoutineName: m.RoutineName}, repo: repo, ctx: ctx, log: log, manifest: m}
	a.emitter = NewEmitter(m.AttemptID, w.client, log, w.clock, m.NextSeq, m.ElapsedBeforeUS)
	git, ierr := w.runner.git.Inspect(ctx, m.WorktreePath, m.BaseCommit)
	cleanup := a.cleanup(ctx, model.Failed, git, ierr != nil)
	switch cleanup.Outcome {
	case "removed", "missing":
		rep.Cleaned++
	default:
		rep.Retained++
	}
	w.noteRetention(m)
	// 3 (continued). Report.
	if !daemonKnows {
		log.WarnContext(ctx, "daemon does not know this attempt; manifest resolved locally", "status", se.Status)
		return nil
	}
	flushCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	a.emitter.Lifecycle("reconciled after worker restart", map[string]any{"cleanup": cleanup.Outcome, "reason": cleanup.Reason})
	a.emitter.Flush(flushCtx)
	if st.Terminal {
		return w.client.PatchCleanup(flushCtx, m.AttemptID, protocol.CleanupPatch{Git: &git, Cleanup: cleanup})
	}
	_, err = w.client.Complete(flushCtx, m.AttemptID, protocol.CompleteRequest{
		LeaseToken: "", State: model.Failed, FailureReason: model.ReasonWorkerRestart, ExitCode: -1, Git: git, Cleanup: cleanup,
		SessionID: m.SessionID, Launches: m.Launches, StartedAt: m.CreatedAt, FinishedAt: w.clock().UTC(),
	})
	if err != nil {
		// 409: the sweeper closed the lease. 400: the daemon requires a lease
		// token, which manifests never store (tokens are not persisted), so a
		// worker_restart completion cannot present one. Either way the target's
		// state is the daemon's business; the cleanup fields still land.
		if errors.As(err, &se) && (se.Status == 400 || se.Status == 409) {
			return w.client.PatchCleanup(flushCtx, m.AttemptID, protocol.CleanupPatch{Git: &git, Cleanup: cleanup})
		}
		return err
	}
	return nil
}
