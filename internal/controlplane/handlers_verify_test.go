package controlplane

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/modes"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

// fakeMode is the smallest modes.Mode the verification flow needs.
type fakeMode struct {
	name   string
	level  model.VerificationLevel
	writes model.WriteScope
	follow func(env *protocol.ResultEnvelope, c modes.FollowUpContext) []modes.WorkSpec
}

func (m fakeMode) Name() string                    { return m.name }
func (m fakeMode) Preamble() string                { return "test preamble" }
func (m fakeMode) AllowedTools() []string          { return nil }
func (m fakeMode) ResultSchema() json.RawMessage   { return nil }
func (m fakeMode) Level() model.VerificationLevel  { return m.level }
func (m fakeMode) Checkpoints() []string           { return nil }
func (m fakeMode) DefaultClass() model.BudgetClass { return model.ClassNormal }
func (m fakeMode) DefaultAutonomy() model.Autonomy { return model.AutonomyAuto }
func (m fakeMode) Writes() model.WriteScope        { return m.writes }
func (m fakeMode) FollowUps(env *protocol.ResultEnvelope, c modes.FollowUpContext) []modes.WorkSpec {
	if m.follow == nil {
		return nil
	}
	return m.follow(env, c)
}

// buildMode is an L2 mode whose FollowUps spawns one verify Work for the
// finished attempt, like implement/greenfield will.
func buildMode() fakeMode {
	return fakeMode{name: "build", level: model.L2, writes: model.WritesRepo,
		follow: func(_ *protocol.ResultEnvelope, c modes.FollowUpContext) []modes.WorkSpec {
			return []modes.WorkSpec{{
				Mode: "verify", Repository: c.Repository, Title: "verify: build", Prompt: "re-check the claims",
				Class: c.Class, Priority: c.Priority,
				VerifyOf: &modes.VerifySubject{AttemptID: c.AttemptID, Branch: c.Branch, Head: c.Head, UI: c.UI},
			}}
		}}
}

// newVerifyHarness is newHarness with a mode registry wired (server_test.go's
// fixture stays untouched).
func newVerifyHarness(t *testing.T, all []modes.Mode) *harness {
	t.Helper()
	clock := &fakeClock{now: time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)}
	st, err := store.Open(context.Background(), t.TempDir()+"/forge.sqlite3", store.Options{Clock: clock.Now})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	})
	if err := st.Write(context.Background(), func(tx *store.Tx) error { return tx.EnsureProject(context.Background(), "default") }); err != nil {
		t.Fatal(err)
	}
	reg, err := modes.NewRegistry(all)
	if err != nil {
		t.Fatal(err)
	}
	levels := "info"
	srv, err := NewServer(ServerOptions{Store: st, Clock: clock.Now, Version: "test", Token: testToken, TransportOverride: transportUnix, Modes: reg,
		LogLevels: func() string { return levels }, SetLogLevels: func(spec string) error { levels = spec; return nil }})
	if err != nil {
		t.Fatal(err)
	}
	hs := httptest.NewServer(srv.Handler())
	t.Cleanup(hs.Close)
	return &harness{t: t, st: st, srv: srv, http: hs, clock: clock, leases: map[string]string{}}
}

// submitTask creates one ad-hoc task in the given mode.
func submitTask(h *harness, mode string) workCreated {
	h.t.Helper()
	var out workCreated
	// Force: the fixture resubmits one prompt on purpose; M11 intake dedupe
	// would otherwise refuse the second submission.
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "do the thing", Repositories: []string{"equitizr"}, Mode: mode, Force: true}, &out, http.StatusCreated)
	return out
}

const subjectHead = "feedfacefeedfacefeedfacefeedfacefeedface"

// completeSubject reports a succeeded attempt whose worker-side L0/L1 passed.
func completeSubject(h *harness, c *protocol.Claim) protocol.CompleteResponse {
	h.t.Helper()
	req := completeRequest(model.Succeeded, h.clock.Now())
	req.Git.Head = subjectHead
	req.Result = json.RawMessage(`{"schema_version":1,"summary":"built","needs_input":null,"changes":[],"checks_run":[],"claims":[{"claim":"it works","evidence":"ran it"}],"commits":["abc"]}`)
	req.Verification = protocol.Verification{Level: 1, Passed: true, Verdict: json.RawMessage(`{"l1_vacuous":true}`)}
	return h.complete(c, req)
}

// completeVerdict reports the verify attempt with the given verdict extra.
func completeVerdict(h *harness, c *protocol.Claim, verdict string, artifacts []protocol.ArtifactUpload) protocol.CompleteResponse {
	h.t.Helper()
	req := completeRequest(model.Succeeded, h.clock.Now())
	req.Git = protocol.GitOutcome{Head: subjectHead} // no writes: L0 of the verify attempt
	req.Result = json.RawMessage(`{"schema_version":1,"summary":"verified","needs_input":null,"changes":[],"checks_run":[],"claims":[],"verdict":"` + verdict + `","claims_checked":[]}`)
	req.Verification = protocol.Verification{Level: 1, Passed: true}
	req.Artifacts = artifacts
	return h.complete(c, req)
}

func TestVerifyFlowL2Pass(t *testing.T) {
	h := newVerifyHarness(t, []modes.Mode{buildMode(), fakeMode{name: "verify", level: model.L0, writes: model.WritesNone}})
	h.register(testWorkerID)
	created := submitTask(h, "build")

	c := h.mustClaim("r1")
	if c.ModeInfo == nil || c.ModeInfo.WriteScope != model.WritesRepo || c.ModeInfo.RequiredLevel != 2 || c.VerifyOf != nil {
		t.Fatalf("subject claim mode_info = %+v verify_of = %+v", c.ModeInfo, c.VerifyOf)
	}
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 7)
	if done := completeSubject(h, c); done.State != model.Verifying {
		t.Fatalf("subject complete = %+v", done)
	}
	if tg := h.target(c.TargetID); tg.State != model.Verifying {
		t.Fatalf("subject target = %+v", tg)
	}

	// The mode's follow-up: one routine-less verify Work with verify_of in the
	// snapshot.
	work, err := h.st.OpenWork(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	var verifyWork *store.Work
	for i := range work {
		if work[i].RoutineName == "verify" {
			verifyWork = &work[i]
		}
	}
	if verifyWork == nil {
		t.Fatalf("no verify work created; open work = %+v", work)
	}
	if verifyWork.RoutineID != "" || verifyWork.Trigger != model.TriggerDependency || verifyWork.Generation != 0 {
		t.Errorf("verify work = %+v", verifyWork)
	}
	vo := verifyOfFromSnapshot(verifyWork.Snapshot)
	if vo == nil || vo.AttemptID != c.AttemptID || vo.Head != subjectHead {
		t.Fatalf("verify_of in snapshot = %+v", vo)
	}

	// Claiming it hands the worker the subject link and the verify mode's info.
	vc := h.mustClaim("r2")
	if vc.WorkID != verifyWork.ID || vc.Mode != "verify" {
		t.Fatalf("verify claim = %+v", vc)
	}
	if vc.VerifyOf == nil || vc.VerifyOf.AttemptID != c.AttemptID || vc.VerifyOf.Head != subjectHead {
		t.Fatalf("claim verify_of = %+v", vc.VerifyOf)
	}
	if vc.ModeInfo == nil || vc.ModeInfo.WriteScope != model.WritesNone || vc.ModeInfo.RequiredLevel != 0 {
		t.Fatalf("verify claim mode_info = %+v", vc.ModeInfo)
	}

	h.heartbeat(vc, model.Preparing, 0)
	h.heartbeat(vc, model.Running, 8)
	arts := []protocol.ArtifactUpload{{Kind: "screenshot", Path: "/tmp/shot.png", Bytes: 42, SHA256: "aa11"}}
	if done := completeVerdict(h, vc, "pass", arts); done.State != model.Succeeded {
		t.Fatalf("verify complete = %+v", done)
	}

	// The verdict decides the subject.
	if tg := h.target(c.TargetID); tg.State != model.Succeeded {
		t.Fatalf("subject after verdict = %+v", tg)
	}
	var rows []store.Verification
	h.call(http.MethodGet, "/api/v1/verifications?attempt_id="+c.AttemptID, nil, &rows, http.StatusOK)
	if len(rows) != 2 || rows[0].Level != 1 || rows[0].DecidedBy != "worker" || rows[1].Level != 2 || !rows[1].Passed || rows[1].VerifierAttemptID != vc.AttemptID || rows[1].DecidedBy != "verify" {
		t.Fatalf("verifications = %+v", rows)
	}
	if a := h.attempt(c.AttemptID); a.VerificationLevel == nil || *a.VerificationLevel != 2 || a.VerificationPass == nil || !*a.VerificationPass {
		t.Errorf("subject summary columns = %+v", a)
	}
	got, err := h.st.ArtifactsForAttempt(context.Background(), vc.AttemptID)
	if err != nil || len(got) != 1 || got[0].Kind != "screenshot" || got[0].SHA256 != "aa11" || got[0].Bytes != 42 {
		t.Errorf("artifacts = %+v %v", got, err)
	}
	// Facts for the subject were recorded when the verdict landed.
	if facts, err := h.st.FactsForAttempt(context.Background(), c.AttemptID); err != nil || facts.State != model.Succeeded {
		t.Errorf("subject facts = %+v %v", facts, err)
	}
	_ = created
}

func TestVerifyFlowL2Fail(t *testing.T) {
	h := newVerifyHarness(t, []modes.Mode{buildMode(), fakeMode{name: "verify", level: model.L0, writes: model.WritesNone}})
	h.register(testWorkerID)
	submitTask(h, "build")
	c := h.mustClaim("r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 7)
	if done := completeSubject(h, c); done.State != model.Verifying {
		t.Fatalf("subject complete = %+v", done)
	}
	vc := h.mustClaim("r2")
	if vc.VerifyOf == nil {
		t.Fatalf("verify claim = %+v", vc)
	}
	h.heartbeat(vc, model.Preparing, 0)
	h.heartbeat(vc, model.Running, 8)
	completeVerdict(h, vc, "fail", nil)
	tg := h.target(c.TargetID)
	if tg.State != model.Unverified || tg.UnverifiedReason != "verify_verdict:fail" {
		t.Fatalf("subject after fail verdict = %+v", tg)
	}
	rows, err := h.st.VerificationsForAttempt(context.Background(), c.AttemptID)
	if err != nil || len(rows) != 2 || rows[1].Passed {
		t.Errorf("verifications = %+v %v", rows, err)
	}
}

func TestVerifyFlowL3ApproveAndReject(t *testing.T) {
	h := newVerifyHarness(t, []modes.Mode{fakeMode{name: "gated", level: model.L3, writes: model.WritesRepo}})
	h.register(testWorkerID)

	// Approve.
	submitTask(h, "gated")
	c := h.mustClaim("r1")
	if c.ModeInfo == nil || c.ModeInfo.RequiredLevel != 3 {
		t.Fatalf("mode_info = %+v", c.ModeInfo)
	}
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 7)
	if done := completeSubject(h, c); done.State != model.Verifying {
		t.Fatalf("complete = %+v", done)
	}
	var target store.Target
	h.call(http.MethodPost, "/api/v1/targets/"+c.TargetID+"/approve", decisionRequest{By: "nate"}, &target, http.StatusOK)
	if target.State != model.Succeeded {
		t.Fatalf("approved target = %+v", target)
	}
	rows, err := h.st.VerificationsForAttempt(context.Background(), c.AttemptID)
	if err != nil || len(rows) != 2 || rows[1].Level != 3 || !rows[1].Passed || rows[1].DecidedBy != "nate" {
		t.Fatalf("verifications after approve = %+v %v", rows, err)
	}
	// A second decision on a decided target is refused with its state named.
	status, raw := h.do(http.MethodPost, "/api/v1/targets/"+c.TargetID+"/approve", decisionRequest{}, nil, "")
	if status != http.StatusConflict {
		t.Errorf("re-approve = %d %s", status, raw)
	}

	// Reject.
	submitTask(h, "gated")
	c2 := h.mustClaim("r2")
	h.heartbeat(c2, model.Preparing, 0)
	h.heartbeat(c2, model.Running, 9)
	if done := completeSubject(h, c2); done.State != model.Verifying {
		t.Fatalf("second complete = %+v", done)
	}
	h.call(http.MethodPost, "/api/v1/targets/"+c2.TargetID+"/reject", decisionRequest{By: "nate", Reason: "does not build"}, &target, http.StatusOK)
	if target.State != model.Unverified || target.UnverifiedReason != "human_rejected" {
		t.Fatalf("rejected target = %+v", target)
	}
	if a := h.attempt(c2.AttemptID); a.VerificationLevel == nil || *a.VerificationLevel != 3 || *a.VerificationPass {
		t.Errorf("summary after reject = %+v", a)
	}
}

// TestReverifyEndpoint: an unverified subject goes back to verifying with a
// fresh verify follow-up, and the new verdict decides it — without the
// subject ever re-running.
func TestReverifyEndpoint(t *testing.T) {
	h := newVerifyHarness(t, []modes.Mode{buildMode(), fakeMode{name: "verify", level: model.L0, writes: model.WritesNone}})
	h.register(testWorkerID)
	submitTask(h, "build")
	c := h.mustClaim("r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 7)
	if done := completeSubject(h, c); done.State != model.Verifying {
		t.Fatalf("subject complete = %+v", done)
	}
	// First verify fails its verdict: subject lands unverified.
	vc := h.mustClaim("r2")
	h.heartbeat(vc, model.Preparing, 0)
	h.heartbeat(vc, model.Running, 8)
	completeVerdict(h, vc, "fail", nil)
	if tg := h.target(c.TargetID); tg.State != model.Unverified {
		t.Fatalf("subject after fail verdict = %+v", tg)
	}

	var got store.Target
	h.call(http.MethodPost, "/api/v1/targets/"+c.TargetID+"/reverify", nil, &got, http.StatusOK)
	if got.ID != c.TargetID || got.State != model.Verifying || got.UnverifiedReason != "" {
		t.Fatalf("reverified target = %+v", got)
	}

	// A fresh verify Work exists, claimable, linked to the same subject attempt.
	vc2 := h.mustClaim("r3")
	if vc2.Mode != "verify" || vc2.VerifyOf == nil || vc2.VerifyOf.AttemptID != c.AttemptID || vc2.VerifyOf.Head != subjectHead {
		t.Fatalf("second verify claim = %+v verify_of = %+v", vc2, vc2.VerifyOf)
	}
	h.heartbeat(vc2, model.Preparing, 0)
	h.heartbeat(vc2, model.Running, 6)
	completeVerdict(h, vc2, "pass", nil)
	if tg := h.target(c.TargetID); tg.State != model.Succeeded {
		t.Fatalf("subject after reverify pass = %+v", tg)
	}
}

// TestReverifyEndpointRefusals: only an unverified target may reverify.
func TestReverifyEndpointRefusals(t *testing.T) {
	h := newVerifyHarness(t, []modes.Mode{buildMode(), fakeMode{name: "verify", level: model.L0, writes: model.WritesNone}})
	h.register(testWorkerID)
	submitTask(h, "build")
	c := h.mustClaim("r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 7)
	// verifying, not unverified: 409.
	if done := completeSubject(h, c); done.State != model.Verifying {
		t.Fatalf("subject complete = %+v", done)
	}
	if status, body := h.do(http.MethodPost, "/api/v1/targets/"+c.TargetID+"/reverify", nil, nil, ""); status != http.StatusConflict {
		t.Fatalf("reverify of a verifying target = %d %s, want 409", status, body)
	}
	if status, body := h.do(http.MethodPost, "/api/v1/targets/ffffffffffffffffffffffffffffffff/reverify", nil, nil, ""); status != http.StatusNotFound {
		t.Fatalf("unknown target = %d %s, want 404", status, body)
	}
	if status, body := h.do(http.MethodPost, "/api/v1/targets/not-an-id/reverify", nil, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("invalid id = %d %s, want 400", status, body)
	}
}
