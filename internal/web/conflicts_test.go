package web

import (
	"context"
	"net/http"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// Conflict auto-recovery: first conflict → automatic requeue with a fresh
// rebase budget; second conflict → the work is cancelled so dependants and
// the supervise settle instead of waiting forever.
func TestConflictAutoRecovery(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	var created workCreated
	h.call(http.MethodPost, "/api/v1/tasks", map[string]any{
		"prompt": "conflicting change", "repositories": []string{"equitizr"}, "integrate": true,
	}, &created, http.StatusCreated)
	target := created.Targets[0].ID

	c := h.mustClaim("cr1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 4321)
	h.complete(c, completeRequest(model.Succeeded, h.clock.now))

	toConflict := func() {
		if err := h.st.Write(context.Background(), func(tx *store.Tx) error {
			if _, err := tx.Transition(context.Background(), target, model.Merging, store.TransitionOptions{Actor: "test"}); err != nil {
				return err
			}
			_, err := tx.Transition(context.Background(), target, model.Conflict, store.TransitionOptions{Actor: "test"})
			return err
		}); err != nil {
			t.Fatal(err)
		}
	}
	state := func() model.State {
		t.Helper()
		row, err := h.st.TargetsForWork(context.Background(), created.Work.ID)
		if err != nil || len(row) != 1 {
			t.Fatalf("targets = %v, %v", row, err)
		}
		return row[0].State
	}

	toConflict()
	// Inside the grace: untouched.
	h.srv.recoverConflicts(context.Background())
	if got := state(); got != model.Conflict {
		t.Fatalf("inside grace = %s", got)
	}
	// Past the grace: auto-requeued.
	h.clock.Advance(conflictGrace + time.Minute)
	h.srv.recoverConflicts(context.Background())
	if got := state(); got != model.QueuedForMerge {
		t.Fatalf("first recovery = %s, want queued_for_merge", got)
	}

	// Second conflict: the work is cancelled.
	toConflict()
	h.clock.Advance(conflictGrace + time.Minute)
	h.srv.recoverConflicts(context.Background())
	if got := state(); got != model.Cancelled {
		t.Fatalf("second recovery = %s, want cancelled", got)
	}
}
