package web

import (
	"context"
	"encoding/json"
	"net/http"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/modes"
	"forge/internal/core/store"
)

// planMode is a level-1, non-writing mode whose successful result carries a
// tasks array; afterComplete turns that into the plan batch.
func planMode() fakeMode {
	return fakeMode{name: "plan", level: model.L1, writes: model.WritesNone}
}

// TestPlanFanOutStampsProvenance drives a plan attempt to success and asserts
// each spawned task points back at the plan Work (caused_by + cause plan_task)
// and shares its root.
func TestPlanFanOutStampsProvenance(t *testing.T) {
	h := newVerifyHarness(t, []modes.Mode{planMode()})
	h.register(testWorkerID)
	created := submitTask(h, "plan")

	c := h.mustClaim("r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 7)
	req := completeRequest(model.Succeeded, h.clock.Now())
	req.Result = json.RawMessage(`{"schema_version":1,"summary":"planned","tasks":[{"title":"first","prompt":"do first"},{"title":"second","prompt":"do second","blocked_by":[0]}]}`)
	if done := h.complete(c, req); done.State != model.Succeeded {
		t.Fatalf("plan complete = %+v", done)
	}

	work, err := h.st.OpenWork(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	var tasks []store.Work
	for i := range work {
		if work[i].RoutineName == "plan-task" {
			tasks = append(tasks, work[i])
		}
	}
	if len(tasks) != 2 {
		t.Fatalf("plan tasks = %d, want 2", len(tasks))
	}
	for _, task := range tasks {
		if task.CausedByWorkID != created.Work.ID {
			t.Errorf("task %s caused_by = %q, want plan %q", task.Title, task.CausedByWorkID, created.Work.ID)
		}
		if task.Cause != model.CausePlanTask {
			t.Errorf("task %s cause = %q, want plan_task", task.Title, task.Cause)
		}
		if task.RootWorkID != created.Work.ID {
			t.Errorf("task %s root = %q, want plan %q", task.Title, task.RootWorkID, created.Work.ID)
		}
		if task.PlanBatchID != created.Work.ID {
			t.Errorf("task %s plan_batch_id = %q, want %q (kept alongside)", task.Title, task.PlanBatchID, created.Work.ID)
		}
	}
}

// TestVerifyFollowUpStampsProvenance runs the L2 verify flow and asserts the
// verify Work is caused_by the subject Work with cause verify, so the audit
// link is a first-class column, not only snapshot JSON.
func TestVerifyFollowUpStampsProvenance(t *testing.T) {
	h := newVerifyHarness(t, []modes.Mode{buildMode(), fakeMode{name: "verify", level: model.L0, writes: model.WritesNone}})
	h.register(testWorkerID)
	created := submitTask(h, "build")

	c := h.mustClaim("r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 7)
	if done := completeSubject(h, c); done.State != model.Verifying {
		t.Fatalf("subject complete = %+v", done)
	}

	work, err := h.st.OpenWork(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	var verify *store.Work
	for i := range work {
		if work[i].RoutineName == "verify" {
			verify = &work[i]
		}
	}
	if verify == nil {
		t.Fatal("no verify work created")
	}
	if verify.CausedByWorkID != created.Work.ID {
		t.Errorf("verify caused_by = %q, want subject %q", verify.CausedByWorkID, created.Work.ID)
	}
	if verify.Cause != model.CauseVerify {
		t.Errorf("verify cause = %q, want verify", verify.Cause)
	}
	if verify.RootWorkID != created.Work.ID {
		t.Errorf("verify root = %q, want subject %q", verify.RootWorkID, created.Work.ID)
	}
}

// TestLineageEndpoint asserts GET .../lineage resolves any member id to its
// root and returns a multi-level tree with correct depths and internal
// dependency edges.
func TestLineageEndpoint(t *testing.T) {
	h := newVerifyHarness(t, []modes.Mode{planMode()})
	h.register(testWorkerID)
	created := submitTask(h, "plan")
	c := h.mustClaim("r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 7)
	req := completeRequest(model.Succeeded, h.clock.Now())
	req.Result = json.RawMessage(`{"schema_version":1,"summary":"planned","tasks":[{"title":"first","prompt":"do first"},{"title":"second","prompt":"do second","blocked_by":[0]}]}`)
	h.complete(c, req)

	// Find a child so we can prove any member id resolves to the same root.
	work, err := h.st.OpenWork(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	var childID string
	for i := range work {
		if work[i].RoutineName == "plan-task" && work[i].Title == "second" {
			childID = work[i].ID
		}
	}
	if childID == "" {
		t.Fatal("no child task found")
	}

	for _, from := range []string{created.Work.ID, childID} {
		var resp lineageResponse
		h.call(http.MethodGet, "/api/v1/work/"+from+"/lineage", nil, &resp, http.StatusOK)
		if resp.RootID != created.Work.ID {
			t.Errorf("from %s: root_id = %q, want %q", from[:8], resp.RootID, created.Work.ID)
		}
		if len(resp.Nodes) != 4 {
			t.Fatalf("from %s: %d nodes, want 4 (plan + 2 tasks + continuation)", from[:8], len(resp.Nodes))
		}
		depth := map[string]int{}
		for _, n := range resp.Nodes {
			depth[n.Work.ID] = n.Depth
		}
		if depth[created.Work.ID] != 0 {
			t.Errorf("plan depth = %d, want 0", depth[created.Work.ID])
		}
		if depth[childID] != 1 {
			t.Errorf("child depth = %d, want 1", depth[childID])
		}
		// The blocked_by edge between the two tasks, plus the continuation's
		// on:terminal edge on each task.
		if len(resp.DependencyEdges) != 3 {
			t.Errorf("dependency edges = %d, want 3", len(resp.DependencyEdges))
		}
	}
}

// A malformed plan (self-blocking task) fails the ATTEMPT with the error on
// record — it must never 500 the completion into a lease-expiry death.
func TestPlanInvalidFailsAttemptNotTransport(t *testing.T) {
	h := newVerifyHarness(t, []modes.Mode{planMode()})
	h.register(testWorkerID)
	created := submitTask(h, "plan")

	c := h.mustClaim("r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 5)
	req := completeRequest(model.Succeeded, h.clock.Now())
	req.Result = json.RawMessage(`{"schema_version":1,"summary":"planned","tasks":[{"title":"a","prompt":"p","blocked_by":[0]}]}`)
	done := h.complete(c, req)
	if done.State != model.Failed {
		t.Fatalf("complete = %+v, want failed (not a 500)", done)
	}
	tg := h.target(created.Targets[0].ID)
	if tg.State != model.Failed || tg.FailureReason != model.ReasonResultUnparseable {
		t.Errorf("target = %s/%s, want failed/result_unparseable", tg.State, tg.FailureReason)
	}
	entries, err := h.st.JournalForEntity(context.Background(), "target", created.Targets[0].ID)
	if err != nil || !hasKind(entries, "plan.invalid") {
		t.Errorf("journal lacks plan.invalid (%v)", err)
	}
}
