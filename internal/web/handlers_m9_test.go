package web

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/modes"
	planmode "forge/internal/core/modes/plan"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

// submitM9 creates one ad-hoc task with M9 knobs.
func submitM9(h *harness, prompt string, req workRequest) workCreated {
	h.t.Helper()
	req.Prompt, req.Force = prompt, true
	if len(req.Repositories) == 0 {
		req.Repositories = []string{"equitizr"}
	}
	var out workCreated
	h.call(http.MethodPost, "/api/v1/tasks", req, &out, http.StatusCreated)
	return out
}

// An integrating task: facts land at succeeded, the Target moves to
// queued_for_merge, and the write-set columns are filled from the envelope.
func TestCompleteEnqueuesMergeAndRecordsFacts(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	created := submitM9(h, "integrate me", workRequest{Integrate: true, Paths: []string{"docs/**"}})
	c := h.mustClaim("m9-int-1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 5)
	req := completeRequest(model.Succeeded, h.clock.Now())
	req.Result = json.RawMessage(`{"schema_version":1,"summary":"done","needs_input":null,"changes":[{"path":"docs/a.md","kind":"modified"},{"path":"internal/oops.go","kind":"modified"}],"checks_run":[],"claims":[]}`)
	done := h.complete(c, req)
	if done.State != model.QueuedForMerge {
		t.Fatalf("complete state = %v, want queued_for_merge", done.State)
	}
	facts, err := h.st.FactsForAttempt(context.Background(), c.AttemptID)
	if err != nil {
		t.Fatalf("facts must exist at succeeded for integrating work: %v", err)
	}
	if facts.State != model.Succeeded {
		t.Errorf("facts state = %v, want succeeded", facts.State)
	}
	if len(facts.DeclaredPaths) != 1 || facts.DeclaredPaths[0] != "docs/**" {
		t.Errorf("declared = %v", facts.DeclaredPaths)
	}
	if len(facts.TouchedPaths) != 2 {
		t.Errorf("touched = %v", facts.TouchedPaths)
	}
	if facts.WriteSetPrecision == nil || *facts.WriteSetPrecision != 0.5 {
		t.Errorf("precision = %v, want 0.5", facts.WriteSetPrecision)
	}
	if facts.MergeOutcome != "" || facts.MergeWaitUS != nil {
		t.Errorf("merge columns must stay NULL until the integrator fills them: %+v", facts)
	}
	_ = created
}

// Overlapping write sets serialise with path_lease visible in the queue;
// disjoint ones are both claimable. lease_wait_us is derivable afterwards.
func TestPathLeaseServializesClaims(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	first := submitM9(h, "hold internal", workRequest{Paths: []string{"internal/**"}})
	overlap := submitM9(h, "also internal", workRequest{Paths: []string{"internal/model/**"}})
	disjoint := submitM9(h, "docs only", workRequest{Paths: []string{"docs/**"}})

	c1 := h.mustClaim("m9-l1")
	if c1.WorkID != first.Work.ID {
		t.Fatalf("first claim = %+v", c1)
	}
	// The overlapping task is passed over; the disjoint one is claimable.
	c2 := h.mustClaim("m9-l2")
	if c2.WorkID != disjoint.Work.ID {
		t.Fatalf("second claim should be the disjoint task, got %+v", c2)
	}
	h.clock.Advance(3 * time.Second)
	if status, _ := h.claim(testWorkerID, "m9-l3"); status != http.StatusNoContent {
		t.Fatalf("overlapping task must wait, got %d", status)
	}
	var rows []queueItem
	h.call(http.MethodGet, "/api/v1/queue", nil, &rows, http.StatusOK)
	found := ""
	for _, r := range rows {
		if r.Work.ID == overlap.Work.ID {
			found = r.Reason
		}
	}
	if !strings.HasPrefix(found, "path_lease held by ") {
		t.Fatalf("queue reason = %q", found)
	}
	// Finish the holder; the overlapping task is claimable and its facts
	// carry lease_wait_us.
	h.heartbeat(c1, model.Preparing, 0)
	h.heartbeat(c1, model.Running, 5)
	h.complete(c1, completeRequest(model.Failed, h.clock.Now()))
	c3 := h.mustClaim("m9-l4")
	if c3.WorkID != overlap.Work.ID {
		t.Fatalf("post-release claim = %+v", c3)
	}
	h.heartbeat(c3, model.Preparing, 0)
	h.heartbeat(c3, model.Running, 6)
	h.complete(c3, completeRequest(model.Failed, h.clock.Now()))
	facts, err := h.st.FactsForAttempt(context.Background(), c3.AttemptID)
	if err != nil {
		t.Fatal(err)
	}
	if facts.LeaseWaitUS == nil || *facts.LeaseWaitUS <= 0 {
		t.Fatalf("lease_wait_us = %v, want > 0 (blocked at m9-l2, claimed after the advance)", facts.LeaseWaitUS)
	}
}

// deps + integrate is refused loudly (M9 known gap: no deps pre-step).
func TestDepsWithIntegrateRefused(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	rt := store.Routine{Name: "depsy", Mode: "run", Prompt: "add left-pad to {{repo}}", Repositories: []string{"equitizr"}, Model: "haiku", TimeoutSeconds: 300, Deps: []string{"left-pad"}, Integrate: true}
	h.call(http.MethodPost, "/api/v1/routines", rt, nil, http.StatusCreated)
	status, body := h.do(http.MethodPost, "/api/v1/routines/depsy/run", nil, nil, "")
	if status != http.StatusBadRequest || !strings.Contains(string(body), "M9 known gap") {
		t.Fatalf("deps+integrate run = %d %s", status, body)
	}
}

// requeue: conflict → queued_for_merge, and nothing else.
func TestRequeueTarget(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	created := submitM9(h, "conflicted", workRequest{Integrate: true, Paths: []string{"a/**"}})
	c := h.mustClaim("m9-rq")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 5)
	h.complete(c, completeRequest(model.Succeeded, h.clock.Now()))
	id := created.Targets[0].ID
	err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		if _, err := tx.Transition(context.Background(), id, model.Merging, store.TransitionOptions{Actor: "integrator"}); err != nil {
			return err
		}
		_, err := tx.Transition(context.Background(), id, model.Conflict, store.TransitionOptions{Actor: "integrator"})
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	var out store.Target
	h.call(http.MethodPost, "/api/v1/targets/"+id+"/requeue", nil, &out, http.StatusOK)
	if out.State != model.QueuedForMerge {
		t.Fatalf("requeue = %+v", out)
	}
	if status, _ := h.do(http.MethodPost, "/api/v1/targets/"+id+"/requeue", nil, nil, ""); status != http.StatusConflict {
		t.Fatalf("double requeue = %d, want 409", status)
	}
}

// A successful plan attempt creates its batch in the same transaction: one
// Work per task with paths, plan_batch_id, inherited integrate/autonomy, and
// blocked_by/stack_on edges from the indexes.
func TestPlanBatchCreated(t *testing.T) {
	h := newVerifyHarness(t, []modes.Mode{planmode.New(), fakeMode{name: "run", level: model.L1, writes: model.WritesRepo}})
	h.register(testWorkerID)
	var created workCreated
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "split the goal", Repositories: []string{"equitizr"}, Mode: "plan", Integrate: true, Force: true}, &created, http.StatusCreated)
	c := h.mustClaim("m9-plan")
	if c.Mode != "plan" {
		t.Fatalf("claim mode = %q", c.Mode)
	}
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 5)
	req := completeRequest(model.Succeeded, h.clock.Now())
	req.Git = protocol.GitOutcome{} // plan writes nothing
	req.Verification = protocol.Verification{Level: 0, Passed: true}
	req.Result = json.RawMessage(`{"schema_version":1,"summary":"three tasks","needs_input":null,"changes":[],"checks_run":[],"claims":[],
		"tasks":[
		 {"title":"api","prompt":"build the api","paths":["internal/api/**"],"size":"M","tier":1},
		 {"title":"ui","prompt":"build the ui","paths":["ui/**"],"size":"S","tier":1},
		 {"title":"wire","prompt":"wire them","paths":["cmd/**"],"blocked_by":[0,1],"stack_on":true,"size":"S","tier":2}]}`)
	done := h.complete(c, req)
	if done.State != model.Succeeded {
		t.Fatalf("plan complete = %+v", done)
	}
	open, err := h.st.OpenWork(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	var batch []store.Work
	for _, w := range open {
		if w.PlanBatchID == created.Work.ID {
			batch = append(batch, w)
		}
	}
	if len(batch) != 3 {
		t.Fatalf("batch = %d works, want 3 (%+v)", len(batch), open)
	}
	byTitle := map[string]store.Work{}
	for _, w := range batch {
		byTitle[w.Title] = w
		if !w.Integrate || w.Trigger != model.TriggerDependency || w.RoutineName != "plan-task" {
			t.Errorf("batch work = %+v", w)
		}
	}
	if p := byTitle["api"].Paths; len(p) != 1 || p[0] != "internal/api/**" {
		t.Errorf("api paths = %v", p)
	}
	if byTitle["wire"].Tier == nil || *byTitle["wire"].Tier != 2 {
		t.Errorf("wire tier = %v", byTitle["wire"].Tier)
	}
	edges, err := h.st.DependencyEdges(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	stacked, plain := 0, 0
	for _, e := range edges {
		if e.Work != byTitle["wire"].ID {
			continue
		}
		if e.StackOn {
			stacked++
		} else {
			plain++
		}
		if e.On != model.OnSuccess {
			t.Errorf("edge on = %v", e.On)
		}
	}
	if stacked+plain != 2 || stacked != 2 {
		t.Errorf("wire edges: stacked=%d plain=%d, want 2 stack_on edges", stacked, plain)
	}
	// The journal carries the batch.
	rows, err := h.st.JournalForEntity(context.Background(), store.EntityWork, created.Work.ID)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, r := range rows {
		if r.Kind == "plan.batch_created" {
			found = true
		}
	}
	if !found {
		t.Error("plan.batch_created journal row missing")
	}
}

// A stacked claim is pinned to the unmerged dependency's branch head, the
// pinned commit lands on the attempt row, and it survives a claim replay.
func TestStackedClaimCarriesBase(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	depHead := "feedfacefeedfacefeedfacefeedfacefeedface"
	a := submitM9(h, "task A", workRequest{Integrate: true, Paths: []string{"a/**"}})
	cA := h.mustClaim("m9-st-a")
	// The worker reports the branch it created, then completes verified.
	h.call(http.MethodPost, "/api/v1/attempts/"+cA.AttemptID+"/heartbeat", protocol.HeartbeatRequest{LeaseToken: h.leases[cA.AttemptID], Phase: "manifest", State: model.Preparing, Branch: "forge/task-a-1"}, nil, http.StatusOK)
	h.heartbeat(cA, model.Running, 5)
	reqA := completeRequest(model.Succeeded, h.clock.Now())
	reqA.Git.Head = depHead
	if done := h.complete(cA, reqA); done.State != model.QueuedForMerge {
		t.Fatalf("A complete = %+v", done)
	}

	b := submitM9(h, "task B on A", workRequest{Integrate: true, Paths: []string{"b/**"}})
	h.call(http.MethodPatch, "/api/v1/work/"+b.Work.ID, workPatch{AddBlockedBy: []dependency{{WorkID: a.Work.ID, On: model.OnSuccess, StackOn: true}}}, nil, http.StatusOK)
	cB := h.mustClaim("m9-st-b")
	if cB.WorkID != b.Work.ID {
		t.Fatalf("stacked claim = %+v", cB)
	}
	if cB.StackBase == nil || cB.StackBase.Commit != depHead || cB.StackBase.Branch != "forge/task-a-1" || cB.StackBase.Depth != 1 {
		t.Fatalf("stack base = %+v", cB.StackBase)
	}
	if at := h.attempt(cB.AttemptID); at.StackBaseCommit != depHead {
		t.Fatalf("attempt stack_base_commit = %q", at.StackBaseCommit)
	}
	// A replayed claim still carries the pinned commit.
	_, replay := h.claim(testWorkerID, "m9-st-b")
	if replay.StackBase == nil || replay.StackBase.Commit != depHead {
		t.Fatalf("replayed stack base = %+v", replay.StackBase)
	}
}
