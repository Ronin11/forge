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

// The assessment cadence: a settled, scored bench fires a user-trial once;
// when the trial settles, a product-review fires once with the trial's
// summary in its objective. Markers make both idempotent.
func TestAssessmentCadence(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutineWith("user-trial", "trial {{objective}}")
	h.createRoutineWith("product-review", "review {{objective}}")

	// A settled bench root with a supervise score in its tree.
	var created workCreated
	h.call(http.MethodPost, "/api/v1/tasks", map[string]any{
		"prompt": "build it", "repositories": []string{"equitizr"}, "bench_name": "cad",
	}, &created, http.StatusCreated)
	c := h.mustClaim("cd1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 9)
	h.complete(c, completeRequest(model.Succeeded, h.clock.now))
	if err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		_, uerr := tx.Exec(context.Background(), `UPDATE attempt_facts SET score_overall = 4, root_work_id = ? WHERE attempt_id = ?`, created.Work.ID, c.AttemptID)
		return uerr
	}); err != nil {
		t.Fatal(err)
	}

	h.srv.assessmentCadence(context.Background())
	h.srv.assessmentCadence(context.Background()) // idempotent
	tree, err := h.st.WorkTree(context.Background(), created.Work.ID)
	if err != nil {
		t.Fatal(err)
	}
	var trial *store.Work
	trials := 0
	for i := range tree {
		if tree[i].SubmittedBy == "cadence:trial" {
			trial = &tree[i]
			trials++
		}
	}
	if trials != 1 || trial.RoutineName != "user-trial" {
		t.Fatalf("trials fired = %d (%+v)", trials, trial)
	}

	// Settle the trial with a summary; the review fires with it.
	tc := h.mustClaim("cd2")
	h.heartbeat(tc, model.Preparing, 0)
	h.heartbeat(tc, model.Running, 9)
	req := completeRequest(model.Succeeded, h.clock.now)
	h.complete(tc, req)
	if err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		_, uerr := tx.Exec(context.Background(), `UPDATE attempts SET result = '{"summary":"maya gave up at search"}' WHERE id = ?`, tc.AttemptID)
		return uerr
	}); err != nil {
		t.Fatal(err)
	}
	h.srv.assessmentCadence(context.Background())
	h.srv.assessmentCadence(context.Background())
	tree, _ = h.st.WorkTree(context.Background(), created.Work.ID)
	reviews := 0
	for _, w := range tree {
		if w.SubmittedBy == "cadence:review" {
			reviews++
			var snap store.Routine
			if err := json.Unmarshal(w.Snapshot, &snap); err != nil || !strings.Contains(snap.Prompt, "maya gave up at search") {
				t.Fatalf("review objective missing trial summary: %q", snap.Prompt)
			}
		}
	}
	if reviews != 1 {
		t.Fatalf("reviews fired = %d", reviews)
	}
}

// Model escalation end to end: the grant journals the marker and tells the
// agent to hand off; the completed non-success target auto-retries once; the
// routing ladder climbs on the retry.
func TestModelEscalationFlow(t *testing.T) {
	h := newRoutingHarness(t)
	h.registerRunners(testWorkerID)
	h.srv.routing.Ladder = []string{"haiku", "sonnet"}
	h.srv.modelCall = func(_ context.Context, _, user, _ string) (string, error) {
		return `{"action":"extend","amount":1,"rationale":"objective evidence agrees: capability-shaped"}`, nil
	}
	h.writeDirective("escjob", "---\nmode: run\nmodel: haiku\n---\ndo {{repo}}\n")
	r := store.Routine{Name: "escjob", Target: "directive:escjob", Repositories: []string{"equitizr"}, TimeoutSeconds: 300, RequireSandbox: true}
	h.call(http.MethodPost, "/api/v1/routines", r, nil, http.StatusCreated)
	work := h.run("escjob")
	target := work.Targets[0].ID

	c := h.mustClaim("esc1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 77)
	h.seedUsage(c.AttemptID, 10, h.clock.Now()) // past the too-early gate

	out, err := h.srv.AdjudicateBudgetRequest(context.Background(), c.AttemptID, "model", 1, "this needs deeper reasoning than I have")
	if err != nil || out.Decision != "granted" || !strings.Contains(out.Message, "handoff") {
		t.Fatalf("escalation = %+v, %v", out, err)
	}
	// Second ask on the same target is refused.
	if out, _ = h.srv.AdjudicateBudgetRequest(context.Background(), c.AttemptID, "model", 1, "again"); out.Decision != "denied" {
		t.Fatalf("double grant = %+v", out)
	}

	// The handoff completes non-success; the sweep retries once and the
	// ladder escalates the retry to sonnet.
	req := completeRequest(model.Failed, h.clock.now)
	h.complete(c, req)
	h.srv.retryGrantedEscalations(context.Background())
	h.srv.retryGrantedEscalations(context.Background()) // once only
	c2 := h.mustClaim("esc2")
	a2, err := h.st.GetAttempt(context.Background(), c2.AttemptID)
	if err != nil || a2.ModelAlias != "sonnet" || a2.EscalatedFrom != "haiku" {
		t.Fatalf("escalated retry = %+v, %v", a2, err)
	}
	_ = target
}
