// retry.go holds the M11 retry rule (DESIGN.md §22, "forge task retry"): a
// terminal, non-merged Target goes back to pending for a fresh attempt.
package store

import (
	"context"
	"errors"
	"fmt"
	"time"

	"forge/internal/model"
)

// ErrModelOverride is returned when a retry asks for a per-target model
// override: neither the targets table nor the work snapshot carries one, so
// the request is refused rather than silently ignored.
var ErrModelOverride = errors.New("model override not supported yet")

// RetryTarget moves a failed, unverified, or cancelled Target back to pending
// for a fresh attempt. Which states may retry is not decided here: the edge
// lives in model.Transition's table (terminal → pending exists for exactly
// those three states), so an ineligible state — running, succeeded, merged —
// is refused there with model.ErrTransition. Beyond the transition, the retry
// clears what the old outcome left behind (failure reasons, the worker
// pinning, the finished timestamps on the Target and its Work) so the
// scheduler sees an ordinary pending Target again, and journals
// target.retried. modelAlias must be empty (ErrModelOverride otherwise).
func (tx *Tx) RetryTarget(ctx context.Context, targetID, modelAlias string) (*Target, error) {
	if modelAlias != "" {
		return nil, fmt.Errorf("retry target %s: %w", targetID, ErrModelOverride)
	}
	t, err := tx.Transition(ctx, targetID, model.Pending, TransitionOptions{Actor: "human"})
	if err != nil {
		return nil, err
	}
	if _, err := tx.Exec(ctx, `UPDATE targets SET worker_id = NULL, failure_reason = NULL, unverified_reason = NULL, cancel_requested = 0, finished_at = NULL, updated_at = ? WHERE id = ?`,
		formatTime(tx.now), targetID); err != nil {
		return nil, fmt.Errorf("reset target %s for retry: %w", targetID, err)
	}
	// The terminal outcome may have finished the Work; reopen it so the queue
	// and the claim path see the retried Target again.
	if _, err := tx.Exec(ctx, `UPDATE work SET finished_at = NULL WHERE id = ?`, t.WorkID); err != nil {
		return nil, fmt.Errorf("reopen work %s for retry: %w", t.WorkID, err)
	}
	if err := tx.Journal(ctx, "target.retried", EntityTarget, targetID, map[string]any{"work_id": t.WorkID, "model": modelAlias}); err != nil {
		return nil, err
	}
	t.WorkerID, t.CancelRequested = "", false
	t.FailureReason, t.UnverifiedReason = "", ""
	t.FinishedAt = time.Time{}
	return t, nil
}
