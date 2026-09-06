package web

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"
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

// A work blocked on:success of a creator that terminated without success is
// a permanent wedge; the sweep cancels it (and cancellation cascades: the
// next tick sees the cancelled work as a dead blocker in turn).
func TestDeadDependantCascade(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutineWith("wedgy", "do {{objective}}")
	blocker := h.run("wedgy")

	var child, grandchild workCreated
	if err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		var err error
		child, err = h.srv.createWorkTx(context.Background(), tx, workRequest{
			Routine: "wedgy", Objective: "child", Force: true,
			After: []string{blocker.Work.ID}, Autonomy: model.AutonomyAuto,
		})
		if err != nil {
			return err
		}
		grandchild, err = h.srv.createWorkTx(context.Background(), tx, workRequest{
			Routine: "wedgy", Objective: "grandchild", Force: true,
			After: []string{child.Work.ID}, Autonomy: model.AutonomyAuto,
		})
		return err
	}); err != nil {
		t.Fatal(err)
	}

	// The blocker fails (non-success terminal).
	c := h.mustClaim("dd1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 111)
	h.complete(c, completeRequest(model.Failed, h.clock.now))

	h.srv.cancelDeadDependants(context.Background())
	if w, err := h.st.GetWork(context.Background(), child.Work.ID); err != nil || w.FinishedAt.IsZero() {
		t.Fatalf("child not cancelled: %+v, %v", w, err)
	}
	// The cascade reaches the grandchild on the next tick.
	h.srv.cancelDeadDependants(context.Background())
	if w, err := h.st.GetWork(context.Background(), grandchild.Work.ID); err != nil || w.FinishedAt.IsZero() {
		t.Fatalf("grandchild not cancelled: %+v, %v", w, err)
	}
}

// Reverting a merged task files a new integrate task chained cause=revert,
// carrying the pushed range; unmerged work is refused.
func TestRevertWork(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	var created workCreated
	h.call(http.MethodPost, "/api/v1/tasks", map[string]any{
		"prompt": "add the widget", "repositories": []string{"equitizr"}, "integrate": true,
	}, &created, http.StatusCreated)
	target := created.Targets[0].ID

	// Not merged yet: refused.
	if status, _ := h.do(http.MethodPost, "/api/v1/work/"+created.Work.ID+"/revert", nil, nil, testToken); status != http.StatusBadRequest {
		t.Fatalf("revert before merge = %d", status)
	}

	c := h.mustClaim("rv1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 42)
	h.complete(c, completeRequest(model.Succeeded, h.clock.now))
	if err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		if _, err := tx.Transition(context.Background(), target, model.Merging, store.TransitionOptions{Actor: "test"}); err != nil {
			return err
		}
		m, err := tx.BeginMerge(context.Background(), target, "equitizr", "main")
		if err != nil {
			return err
		}
		if err := tx.FinishMerge(context.Background(), m.ID, "merged", "aaaa1111", "bbbb2222", nil); err != nil {
			return err
		}
		_, err = tx.Transition(context.Background(), target, model.Merged, store.TransitionOptions{Actor: "test"})
		return err
	}); err != nil {
		t.Fatal(err)
	}

	var revert workCreated
	h.call(http.MethodPost, "/api/v1/work/"+created.Work.ID+"/revert", nil, &revert, http.StatusCreated)
	if revert.Work.Cause != model.CauseRevert || revert.Work.CausedByWorkID != created.Work.ID {
		t.Fatalf("revert provenance = %+v", revert.Work)
	}
	var snap store.Routine
	if err := json.Unmarshal(revert.Work.Snapshot, &snap); err != nil ||
		!strings.Contains(snap.Prompt, "aaaa1111..bbbb2222") || !strings.Contains(snap.Prompt, "CASCADE") {
		t.Fatalf("revert prompt = %q, %v", snap.Prompt, err)
	}
	if !snap.Integrate {
		t.Fatal("revert task must integrate")
	}
}
