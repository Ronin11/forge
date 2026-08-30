package worker

import (
	"context"
	"errors"
	"fmt"

	"forge/internal/model"
	"forge/internal/protocol"
)

// completer is the slice of Client that reconciliation needs.
type completer interface {
	Complete(ctx context.Context, attemptID string, req protocol.CompleteRequest) error
}

// reconciler resolves manifests that have no live attempt in this process.
type reconciler struct {
	workerID  string
	store     *ManifestStore
	repos     map[string]Repository
	report    completer
	isActive  func(attemptID string) bool
	onRetain  func(Manifest)
	configArg string // appended to the cleanup command
}

// run reconciles every non-final manifest and returns the joined errors.
func (r *reconciler) run(ctx context.Context) (handled int, err error) {
	manifests, loadErr := r.store.LoadAll()
	var errs []error
	if loadErr != nil {
		errs = append(errs, loadErr)
	}
	for _, m := range manifests {
		if m.Lifecycle == LifecycleRetained {
			r.onRetain(m)
			continue
		}
		if m.Final() || r.isActive(m.AttemptID) {
			continue
		}
		handled++
		if err := r.reconcileOne(ctx, m); err != nil {
			errs = append(errs, fmt.Errorf("attempt %s: %w", m.AttemptID, err))
		}
	}
	return handled, errors.Join(errs...)
}

// reconcileOne kills any surviving process, applies the cleanup rules from Git
// state, records the result in the manifest, and reports it.
func (r *reconciler) reconcileOne(ctx context.Context, m Manifest) error {
	if m.PID != 0 {
		if err := killProcessGroup(m.PID, m.ProcessIdentity, terminationGrace); err != nil {
			return fmt.Errorf("stop orphaned process: %w", err)
		}
	}
	req := protocol.CompleteRequest{
		WorkerID: r.workerID, State: model.Failed, Reason: "worker_restart",
		Error: "worker restarted while the attempt was " + m.Lifecycle,
	}
	repo, ok := r.repos[m.Repository]
	if !ok || repo.Path != m.RepositoryPath {
		req.Cleanup = protocol.Cleanup{Outcome: "retained", Reason: "repository no longer registered", Command: r.cleanupCommand(m.AttemptID)}
	} else {
		state, err := repo.InspectWorktree(ctx, m.WorktreePath, m.BaseCommit)
		if err != nil {
			return err
		}
		req.Git = &protocol.GitOutcome{Dirty: state.Dirty, NewCommits: state.NewCommits, Pushed: state.Pushed, HeadCommit: state.Head}
		req.Cleanup = r.dispose(ctx, repo, m, state)
	}
	if _, err := r.store.Update(m.AttemptID, func(v *Manifest) {
		v.PID, v.ProcessIdentity = 0, ""
		v.TerminalState = string(model.Failed)
		applyCleanup(v, req.Cleanup)
	}); err != nil {
		return err
	}
	if req.Cleanup.Outcome == "retained" {
		updated, err := r.store.Load(m.AttemptID)
		if err == nil {
			r.onRetain(updated)
		}
	}
	rctx, cancel := context.WithTimeout(ctx, requestTimeout)
	defer cancel()
	return r.report.Complete(rctx, m.AttemptID, req)
}

// dispose applies DecideCleanup and removes the worktree when allowed.
func (r *reconciler) dispose(ctx context.Context, repo Repository, m Manifest, state WorktreeState) protocol.Cleanup {
	remove, reason := DecideCleanup(state)
	switch {
	case reason == "worktree missing":
		return protocol.Cleanup{Outcome: "missing", Reason: reason}
	case remove:
		if err := repo.RemoveWorktree(ctx, m.WorktreePath, false); err != nil {
			return protocol.Cleanup{Outcome: "retained", Reason: "removal failed: " + boundedText(err.Error(), 500), Command: r.cleanupCommand(m.AttemptID)}
		}
		return protocol.Cleanup{Outcome: "removed", Reason: reason}
	default:
		return protocol.Cleanup{Outcome: "retained", Reason: reason, Command: r.cleanupCommand(m.AttemptID)}
	}
}

func (r *reconciler) cleanupCommand(attemptID string) string {
	return "forge cleanup " + attemptID + r.configArg + " --confirm"
}

func applyCleanup(m *Manifest, c protocol.Cleanup) {
	switch c.Outcome {
	case "retained":
		m.Lifecycle = LifecycleRetained
		m.RetentionReason = c.Reason
		m.CleanupCommand = c.Command
	case "missing":
		if m.Lifecycle == LifecyclePreparing {
			m.Lifecycle = LifecycleNotCreated
		} else {
			m.Lifecycle = LifecycleCleaned
		}
		m.RetentionReason = c.Reason
	default:
		m.Lifecycle = LifecycleCleaned
		m.RetentionReason = ""
	}
}

// retainedFromManifest converts a retained manifest to its advertisement.
func retainedFromManifest(m Manifest) protocol.RetainedWorktree {
	return protocol.RetainedWorktree{AttemptID: m.AttemptID, Repository: m.Repository, Path: m.WorktreePath,
		Branch: m.Branch, Reason: m.RetentionReason, Command: m.CleanupCommand}
}

