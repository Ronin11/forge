package web

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"
	"testing"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/model"
	"forge/internal/core/modes"
	planmode "forge/internal/core/modes/plan"
	supervisemode "forge/internal/core/modes/supervise"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

// superviseHarness wires the real plan + supervise modes and a plain run mode.
func superviseHarness(t *testing.T) *harness {
	t.Helper()
	h := newVerifyHarness(t, []modes.Mode{planmode.New(), supervisemode.New(), fakeMode{name: "run", level: model.L1, writes: model.WritesRepo}})
	h.register(testWorkerID)
	return h
}

// planComplete finishes a claimed plan attempt with the given tasks JSON.
func (h *harness) planComplete(c *protocol.Claim, summary, tasksJSON string) protocol.CompleteResponse {
	h.t.Helper()
	req := completeRequest(model.Succeeded, h.clock.Now())
	req.Git = protocol.GitOutcome{}
	req.Verification = protocol.Verification{Level: 0, Passed: true}
	req.Result = json.RawMessage(`{"schema_version":1,"summary":"` + summary + `","needs_input":null,"changes":[],"checks_run":[],"claims":[],"tasks":` + tasksJSON + `}`)
	return h.complete(c, req)
}

// superviseComplete finishes a claimed supervise attempt.
func (h *harness) superviseComplete(c *protocol.Claim, outcome, tasksJSON string) protocol.CompleteResponse {
	h.t.Helper()
	req := completeRequest(model.Succeeded, h.clock.Now())
	req.Git = protocol.GitOutcome{}
	req.Verification = protocol.Verification{Level: 0, Passed: true}
	body := `{"schema_version":1,"summary":"reviewed","needs_input":null,"changes":[],"checks_run":[],"claims":[],` +
		`"assessment":{"outcome":"` + outcome + `","scores":{"correctness":4,"completeness":3,"quality":4,"effort_fit":5,"overall":4},"weakness":"tests are thin"}`
	if tasksJSON != "" {
		body += `,"tasks":` + tasksJSON
	}
	body += `}`
	req.Result = json.RawMessage(body)
	return h.complete(c, req)
}

// workJournalKinds returns the journal kinds recorded for one Work.
func (h *harness) workJournalKinds(id string) map[string]int {
	h.t.Helper()
	rows, err := h.st.JournalForEntity(context.Background(), store.EntityWork, id)
	if err != nil {
		h.t.Fatal(err)
	}
	out := map[string]int{}
	for _, r := range rows {
		out[r.Kind]++
	}
	return out
}

// The full return path: plan → batch + blocked continuation; the
// continuation releases only when every member is terminal (a failure
// included — that is the repair path), sees the outcomes through
// forge_work_outcomes, revises into round 2, and round 2's done ends the
// subtree with scores that land in facts.
func TestSuperviseLifecycle(t *testing.T) {
	h := superviseHarness(t)
	var created workCreated
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "ship the widget", Repositories: []string{"equitizr"}, Mode: "plan", Size: "L", Force: true}, &created, http.StatusCreated)
	c := h.mustClaim("sup-plan")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 5)
	h.planComplete(c, "two tasks", `[{"title":"api","prompt":"build api","paths":[]},{"title":"ui","prompt":"build ui","paths":[]}]`)

	open, err := h.st.OpenWork(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	var cont *store.Work
	tasks := 0
	for _, w := range open {
		w := w
		switch w.Cause {
		case model.CauseContinuation:
			cont = &w
		case model.CausePlanTask:
			tasks++
		}
	}
	if tasks != 2 || cont == nil {
		t.Fatalf("open after plan = %+v", open)
	}
	if cont.Size != "L" {
		t.Errorf("continuation size = %q, want inherited L", cont.Size)
	}

	// Task 1 succeeds; the continuation stays blocked on task 2.
	c1 := h.mustClaim("sup-t1")
	if c1.Mode != "run" {
		t.Fatalf("first claim mode = %q", c1.Mode)
	}
	h.heartbeat(c1, model.Preparing, 0)
	h.heartbeat(c1, model.Running, 5)
	h.complete(c1, completeRequest(model.Succeeded, h.clock.Now()))
	c2 := h.mustClaim("sup-t2")
	if c2.Mode != "run" || c2.WorkID == c1.WorkID {
		t.Fatalf("second claim = %+v", c2)
	}
	if status, _ := h.claim(testWorkerID, "sup-none"); status == http.StatusOK {
		t.Fatal("continuation claimable while a batch member is open")
	}
	// Task 2 fails: all members terminal → the continuation releases (repair
	// is the point).
	h.heartbeat(c2, model.Preparing, 0)
	h.heartbeat(c2, model.Running, 5)
	h.complete(c2, completeRequest(model.Failed, h.clock.Now()))
	cs := h.mustClaim("sup-c1")
	if cs.Mode != "supervise" || cs.WorkID != cont.ID {
		t.Fatalf("continuation claim = %+v", cs)
	}
	if !strings.Contains(cs.Prompt, "Round 1 of 3") || !strings.Contains(cs.Prompt, "forge_work_outcomes") || !strings.Contains(cs.Prompt, "ship the widget") {
		t.Errorf("continuation prompt = %q", cs.Prompt)
	}

	// The outcomes tool reports the batch's reality.
	status, body := h.bridgeTool(cs.AttemptID, "forge_work_outcomes", map[string]any{})
	if status != http.StatusOK || !strings.Contains(string(body), `"failed"`) || !strings.Contains(string(body), `"succeeded"`) || !strings.Contains(string(body), "api") {
		t.Fatalf("outcomes = %d %s", status, body)
	}
	// Cross-tree scope is refused.
	var other workCreated
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "unrelated", Repositories: []string{"equitizr"}, Mode: "run", Force: true}, &other, http.StatusCreated)
	if status, body := h.bridgeTool(cs.AttemptID, "forge_work_outcomes", map[string]any{"work_id": other.Work.ID}); status != http.StatusBadRequest {
		t.Fatalf("cross-tree = %d %s", status, body)
	}

	// Revise: one corrective task, a fresh round-2 continuation, ancestry intact.
	h.heartbeat(cs, model.Preparing, 0)
	h.heartbeat(cs, model.Running, 5)
	h.superviseComplete(cs, "revise", `[{"title":"fix ui","prompt":"repair the ui build","paths":[]}]`)
	if kinds := h.workJournalKinds(cont.ID); kinds["supervise.assessment"] != 1 || kinds["plan.batch_created"] != 1 {
		t.Fatalf("round-1 journal = %v", kinds)
	}
	open, _ = h.st.OpenWork(context.Background())
	var round2 *store.Work
	var cont2 *store.Work
	for _, w := range open {
		w := w
		if w.PlanBatchID == cont.ID && w.Cause == model.CausePlanTask {
			round2 = &w
		}
		if w.Cause == model.CauseContinuation && w.CausedByWorkID == cont.ID {
			cont2 = &w
		}
	}
	if round2 == nil || cont2 == nil {
		t.Fatalf("round 2 not spawned: %+v", open)
	}

	// Finish round 2; its continuation reports done and the subtree settles.
	// The unrelated work is claimable too, so steer by mode.
	var csecond *protocol.Claim
	for i := 0; i < 3; i++ {
		cc := h.mustClaim("sup-r2-" + string(rune('a'+i)))
		h.heartbeat(cc, model.Preparing, 0)
		h.heartbeat(cc, model.Running, 5)
		if cc.WorkID == round2.ID {
			h.complete(cc, completeRequest(model.Succeeded, h.clock.Now()))
			continue
		}
		if cc.Mode == "supervise" {
			csecond = cc
			break
		}
		// the unrelated run-mode work
		h.complete(cc, completeRequest(model.Succeeded, h.clock.Now()))
	}
	if csecond == nil {
		t.Fatal("round-2 continuation never claimable")
	}
	if !strings.Contains(csecond.Prompt, "Round 2 of 3") {
		t.Errorf("round-2 prompt = %q", csecond.Prompt)
	}
	h.superviseComplete(csecond, "done", "")
	if kinds := h.workJournalKinds(cont2.ID); kinds["supervise.done"] != 1 {
		t.Fatalf("round-2 journal = %v", kinds)
	}
	open, _ = h.st.OpenWork(context.Background())
	for _, w := range open {
		if w.RootWorkID == created.Work.ID {
			t.Fatalf("subtree still open: %+v", w)
		}
	}
	// The scores landed in facts, with the tree's dimensions.
	facts, err := h.st.FactsSince(context.Background(), h.clock.Now().Add(-time.Hour), h.clock.Now().Add(time.Hour), "")
	if err != nil {
		t.Fatal(err)
	}
	scored, sawRevise, sawDone := false, false, false
	for _, f := range facts {
		if f.Mode != "supervise" || f.RootWorkID != created.Work.ID {
			continue
		}
		if f.ScoreOverall != nil && *f.ScoreOverall == 4 {
			scored = true
		}
		if f.SuperviseOutcome == "revise" && f.SuperviseRound != nil && *f.SuperviseRound == 1 {
			sawRevise = true
		}
		if f.SuperviseOutcome == "done" && f.SuperviseRound != nil && *f.SuperviseRound == 2 {
			sawDone = true
		}
	}
	if !scored || !sawRevise || !sawDone {
		t.Fatalf("supervise facts missing dimensions (scored=%v revise=%v done=%v): %+v", scored, sawRevise, sawDone, facts)
	}
	// The lineage rollup answers for the whole ask.
	var lin lineageResponse
	h.call(http.MethodGet, "/api/v1/work/"+created.Work.ID+"/lineage", nil, &lin, http.StatusOK)
	ru := lin.Rollup
	if ru.Open != 0 || ru.Works != 6 || ru.Attempts < 5 || ru.Scores == nil || !strings.Contains(string(ru.Scores), `"overall":4`) || ru.Weakness == "" || ru.Size != "L" {
		t.Fatalf("rollup = %+v", ru)
	}
}

// The round cap: with supervise_rounds=1 the first continuation is told it is
// final, and a revise anyway journals rounds_exhausted and spawns nothing.
func TestSuperviseRoundCap(t *testing.T) {
	h := superviseHarness(t)
	h.srv.planCfg = config.PlanConfig{SuperviseRounds: 1}
	var created workCreated
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "small thing", Repositories: []string{"equitizr"}, Mode: "plan", Force: true}, &created, http.StatusCreated)
	c := h.mustClaim("cap-plan")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 5)
	h.planComplete(c, "one task", `[{"title":"do it","prompt":"do the thing","paths":[]}]`)
	ct := h.mustClaim("cap-t1")
	h.heartbeat(ct, model.Preparing, 0)
	h.heartbeat(ct, model.Running, 5)
	h.complete(ct, completeRequest(model.Succeeded, h.clock.Now()))
	cs := h.mustClaim("cap-c1")
	if cs.Mode != "supervise" || !strings.Contains(cs.Prompt, "final round") {
		t.Fatalf("continuation = %+v", cs)
	}
	h.heartbeat(cs, model.Preparing, 0)
	h.heartbeat(cs, model.Running, 5)
	h.superviseComplete(cs, "revise", `[{"title":"more","prompt":"more work","paths":[]}]`)
	if kinds := h.workJournalKinds(cs.WorkID); kinds["supervise.rounds_exhausted"] != 1 {
		t.Fatalf("journal = %v", kinds)
	}
	open, _ := h.st.OpenWork(context.Background())
	for _, w := range open {
		if w.RootWorkID == created.Work.ID {
			t.Fatalf("work spawned past the cap: %+v", w)
		}
	}
}

// Nested plans: a plan-mode task decomposes again; the outer continuation is
// re-blocked onto the nested continuation so it cannot release before the
// nested subtree settles, and a plan-mode task past max_nesting is demoted
// to run.
func TestNestedPlanHoldsOuterContinuation(t *testing.T) {
	h := superviseHarness(t)
	var created workCreated
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "big thing", Repositories: []string{"equitizr"}, Mode: "plan", Force: true}, &created, http.StatusCreated)
	c := h.mustClaim("nest-plan")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 5)
	h.planComplete(c, "split", `[{"title":"sub","prompt":"decompose the backend","paths":[],"mode":"plan"}]`)

	// The nested plan claims in plan mode.
	cn := h.mustClaim("nest-sub")
	if cn.Mode != "plan" {
		t.Fatalf("nested claim mode = %q", cn.Mode)
	}
	h.heartbeat(cn, model.Preparing, 0)
	h.heartbeat(cn, model.Running, 5)
	// Its own batch: one leaf plus one over-deep plan task (demoted).
	h.planComplete(cn, "sub-split", `[{"title":"leaf","prompt":"build the leaf","paths":[]},{"title":"deep","prompt":"decompose further","paths":[],"mode":"plan"}]`)
	if kinds := h.workJournalKinds(cn.WorkID); kinds["plan.nesting_capped"] != 1 {
		t.Fatalf("nested journal = %v", kinds)
	}
	open, _ := h.st.OpenWork(context.Background())
	var outerCont, nestedCont *store.Work
	leaves := 0
	for _, w := range open {
		w := w
		if w.Cause == model.CauseContinuation {
			if w.CausedByWorkID == created.Work.ID {
				outerCont = &w
			} else if w.CausedByWorkID == cn.WorkID {
				nestedCont = &w
			}
		}
		if w.Cause == model.CausePlanTask && w.PlanBatchID == cn.WorkID {
			leaves++
			if snap, err := snapshotRoutine(w); err != nil || snap.Mode != "run" {
				t.Errorf("nested task mode = %q (%v)", snap.Mode, err)
			}
		}
	}
	if outerCont == nil || nestedCont == nil || leaves != 2 {
		t.Fatalf("open = %+v", open)
	}
	// The re-block edge: outer continuation waits on the nested one.
	edges, err := h.st.DependencyEdges(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	reblocked := false
	for _, e := range edges {
		if e.Work == outerCont.ID && e.BlockedBy == nestedCont.ID && e.On == model.OnTerminal {
			reblocked = true
		}
	}
	if !reblocked {
		t.Fatalf("no re-block edge outer→nested; edges = %+v", edges)
	}
	// Finish the leaves; only the NESTED continuation may claim next.
	for i := 0; i < 2; i++ {
		cl := h.mustClaim("nest-leaf-" + string(rune('a'+i)))
		if cl.Mode != "run" {
			t.Fatalf("leaf claim = %+v", cl)
		}
		h.heartbeat(cl, model.Preparing, 0)
		h.heartbeat(cl, model.Running, 5)
		h.complete(cl, completeRequest(model.Succeeded, h.clock.Now()))
	}
	ccn := h.mustClaim("nest-cont")
	if ccn.WorkID != nestedCont.ID {
		t.Fatalf("claimed %s, want nested continuation %s", ccn.WorkID, nestedCont.ID)
	}
	h.heartbeat(ccn, model.Preparing, 0)
	h.heartbeat(ccn, model.Running, 5)
	h.superviseComplete(ccn, "done", "")
	cco := h.mustClaim("nest-outer")
	if cco.WorkID != outerCont.ID {
		t.Fatalf("claimed %s, want outer continuation %s", cco.WorkID, outerCont.ID)
	}
}

// The settlement cascade: a batch member blocked on a failed sibling would
// wedge open forever (dependency_failed is not terminal), starving the
// continuation. Settlement cancels it so the repair path fires.
func TestBatchSettlementCascade(t *testing.T) {
	h := superviseHarness(t)
	var created workCreated
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "chain", Repositories: []string{"equitizr"}, Mode: "plan", Force: true}, &created, http.StatusCreated)
	c := h.mustClaim("cas-plan")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 5)
	h.planComplete(c, "chained", `[{"title":"a","prompt":"first","paths":[]},{"title":"b","prompt":"second","paths":[],"blocked_by":[0]}]`)
	ca := h.mustClaim("cas-a")
	h.heartbeat(ca, model.Preparing, 0)
	h.heartbeat(ca, model.Running, 5)
	h.complete(ca, completeRequest(model.Failed, h.clock.Now()))

	// b was cancelled in the same transaction; the continuation is next.
	open, _ := h.st.OpenWork(context.Background())
	for _, w := range open {
		if w.RootWorkID == created.Work.ID && w.Cause == model.CausePlanTask {
			t.Fatalf("batch member still open: %+v", w)
		}
	}
	cs := h.mustClaim("cas-cont")
	if cs.Mode != "supervise" {
		t.Fatalf("post-settlement claim = %+v", cs)
	}
	status, body := h.bridgeTool(cs.AttemptID, "forge_work_outcomes", map[string]any{})
	if status != http.StatusOK || !strings.Contains(string(body), `"failed"`) || !strings.Contains(string(body), `"cancelled"`) {
		t.Fatalf("outcomes = %d %s", status, body)
	}
}
