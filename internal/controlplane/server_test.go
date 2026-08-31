package controlplane

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"forge/internal/model"
	"forge/internal/protocol"
	"forge/internal/store"
)

const (
	testWorkerID = "0123456789abcdef0123456789abcdef"
	otherWorker  = "fedcba9876543210fedcba9876543210"
	testToken    = "worker-token"
)

// fakeClock is the injected clock: tests advance it to expire leases.
type fakeClock struct {
	mu  sync.Mutex // guards now
	now time.Time
}

func (c *fakeClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *fakeClock) Advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.now = c.now.Add(d)
}

// harness is a store in a temp dir behind httptest, with the transport forced.
type harness struct {
	t      *testing.T
	st     *store.Store
	srv    *Server
	http   *httptest.Server
	clock  *fakeClock
	leases map[string]string // attempt id → the lease token of its latest claim
}

func newHarness(t *testing.T, transport string) *harness {
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
	levels := "info"
	srv, err := NewServer(ServerOptions{
		Store: st, Clock: clock.Now, Version: "test", Token: testToken, TransportOverride: transport,
		AllowHosts: []string{"api.anthropic.com"}, GitConfig: map[string]string{"merge.conflictstyle": "zdiff3"},
		LogLevels: func() string { return levels }, SetLogLevels: func(spec string) error { levels = spec; return nil },
	})
	if err != nil {
		t.Fatal(err)
	}
	hs := httptest.NewServer(srv.Handler())
	t.Cleanup(hs.Close)
	return &harness{t: t, st: st, srv: srv, http: hs, clock: clock, leases: map[string]string{}}
}

// do sends one JSON request with an optional bearer token and decodes a 2xx
// body into out; it returns the status and the raw body for error checks.
func (h *harness) do(method, path string, body, out any, token string) (int, []byte) {
	h.t.Helper()
	var reader *bytes.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			h.t.Fatal(err)
		}
		reader = bytes.NewReader(b)
	} else {
		reader = bytes.NewReader(nil)
	}
	req, err := http.NewRequest(method, h.http.URL+path, reader)
	if err != nil {
		h.t.Fatal(err)
	}
	req.Header.Set("Content-Type", "application/json")
	if token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
	}
	resp, err := h.http.Client().Do(req)
	if err != nil {
		h.t.Fatal(err)
	}
	var buf bytes.Buffer
	if _, err := buf.ReadFrom(resp.Body); err != nil {
		h.t.Fatal(err)
	}
	if err := resp.Body.Close(); err != nil {
		h.t.Fatal(err)
	}
	if resp.Header.Get("X-Request-ID") == "" {
		h.t.Errorf("%s %s: no X-Request-ID", method, path)
	}
	if out != nil && resp.StatusCode >= 200 && resp.StatusCode < 300 && resp.StatusCode != http.StatusNoContent {
		if err := json.Unmarshal(buf.Bytes(), out); err != nil {
			h.t.Fatalf("%s %s: decode %q: %v", method, path, buf.String(), err)
		}
	}
	return resp.StatusCode, buf.Bytes()
}

// call is do over the open transport, failing the test unless status matches.
func (h *harness) call(method, path string, body, out any, want int) []byte {
	h.t.Helper()
	status, raw := h.do(method, path, body, out, "")
	if status != want {
		h.t.Fatalf("%s %s = %d %s, want %d", method, path, status, raw, want)
	}
	return raw
}

func (h *harness) register(workerID string) {
	h.t.Helper()
	// sandbox is ready: createRoutine sets require_sandbox, and M11's routing
	// rule (workRequirements) skips such Work on a sandbox-missing worker.
	req := protocol.RegisterRequest{WorkerID: workerID, Name: "laptop", Version: "test", MaxConcurrent: 2, Executors: []string{"claude-code"}, Capabilities: map[string]string{"sandbox": "ready"},
		Repositories: []protocol.Repository{{Name: "equitizr", Path: "/tmp/equitizr", OriginIdentity: "github.com/x/equitizr", Project: "default"}}}
	var resp protocol.RegisterResponse
	h.call(http.MethodPost, "/api/v1/worker/register", req, &resp, http.StatusOK)
	if resp.LogLevels != "info" {
		h.t.Errorf("register log_levels = %q", resp.LogLevels)
	}
}

func (h *harness) createRoutine(name string) {
	h.t.Helper()
	r := store.Routine{Name: name, Mode: "run", Prompt: "list files in {{repo}}", Repositories: []string{"equitizr"}, Model: "haiku", TimeoutSeconds: 300, RequireSandbox: true}
	h.call(http.MethodPost, "/api/v1/routines", r, nil, http.StatusCreated)
}

func (h *harness) run(name string) workCreated {
	h.t.Helper()
	var out workCreated
	h.call(http.MethodPost, "/api/v1/routines/"+name+"/run", nil, &out, http.StatusCreated)
	if len(out.Targets) != 1 || out.Targets[0].State != model.Pending {
		h.t.Fatalf("run: %+v", out)
	}
	return out
}

func (h *harness) claim(workerID, reqID string) (int, *protocol.Claim) {
	h.t.Helper()
	var c protocol.Claim
	status, _ := h.do(http.MethodPost, "/api/v1/worker/claim", protocol.ClaimRequest{WorkerID: workerID, ClaimRequestID: reqID, LeaseToken: "lease-" + reqID}, &c, "")
	if status == http.StatusOK {
		h.leases[c.AttemptID] = "lease-" + reqID
	}
	return status, &c
}

func (h *harness) mustClaim(reqID string) *protocol.Claim {
	h.t.Helper()
	status, c := h.claim(testWorkerID, reqID)
	if status != http.StatusOK || c.AttemptID == "" {
		h.t.Fatalf("claim = %d %+v", status, c)
	}
	return c
}

func (h *harness) heartbeat(c *protocol.Claim, state model.State, pid int) protocol.HeartbeatResponse {
	h.t.Helper()
	var out protocol.HeartbeatResponse
	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/heartbeat", protocol.HeartbeatRequest{LeaseToken: h.leases[c.AttemptID], Phase: string(state), State: state, PID: pid, PIDStart: 7, SessionID: "sess-1"}, &out, http.StatusOK)
	return out
}

// complete sends the terminal report and returns the daemon's answer.
func (h *harness) complete(c *protocol.Claim, req protocol.CompleteRequest) protocol.CompleteResponse {
	h.t.Helper()
	req.LeaseToken = h.leases[c.AttemptID]
	var out protocol.CompleteResponse
	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/complete", req, &out, http.StatusOK)
	return out
}

func (h *harness) target(id string) *store.Target {
	h.t.Helper()
	t, err := h.st.GetTarget(context.Background(), id)
	if err != nil {
		h.t.Fatal(err)
	}
	return t
}

func (h *harness) attempt(id string) *store.Attempt {
	h.t.Helper()
	a, err := h.st.GetAttempt(context.Background(), id)
	if err != nil {
		h.t.Fatal(err)
	}
	return a
}

// completeRequest is a plausible terminal report; the harness fills the lease.
func completeRequest(state model.State, now time.Time) protocol.CompleteRequest {
	return protocol.CompleteRequest{State: state, NumTurns: 3, Usage: protocol.Usage{InputTokens: 10, OutputTokens: 20}, Launches: 1,
		Git: protocol.GitOutcome{Head: "abc", Commits: 1}, Verification: protocol.Verification{Level: 1, Passed: true}, Cleanup: protocol.Cleanup{Outcome: "removed", Reason: "clean"},
		StartedAt: now.Add(-time.Minute), FinishedAt: now, SessionID: "sess-1"}
}

func TestHandshakeAndLogLevel(t *testing.T) {
	h := newHarness(t, transportUnix)
	var hs protocol.Handshake
	h.call(http.MethodGet, "/api/v1/handshake", nil, &hs, http.StatusOK)
	if hs.Version != "test" || hs.SchemaVersion != h.st.SchemaVersion() || hs.State != "running" || hs.PID == 0 {
		t.Errorf("handshake = %+v", hs)
	}
	var lv logLevelBody
	h.call(http.MethodPost, "/api/v1/log-level", logLevelBody{Levels: "debug,store=trace"}, &lv, http.StatusOK)
	h.call(http.MethodGet, "/api/v1/log-level", nil, &lv, http.StatusOK)
	if lv.Levels != "debug,store=trace" {
		t.Errorf("log-level = %+v", lv)
	}
	if raw := h.call(http.MethodGet, "/healthz", nil, nil, http.StatusOK); string(raw) != "ok" {
		t.Errorf("healthz = %q", raw)
	}
}

func TestRegisterAndRoutines(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	var workers []store.Worker
	h.call(http.MethodGet, "/api/v1/workers", nil, &workers, http.StatusOK)
	if len(workers) != 1 || !workers[0].Connected || workers[0].ID != testWorkerID {
		t.Errorf("workers = %+v", workers)
	}
	var repos []store.Repository
	h.call(http.MethodGet, "/api/v1/repositories", nil, &repos, http.StatusOK)
	if len(repos) != 1 || repos[0].WorkerID != testWorkerID || repos[0].Project != "default" {
		t.Errorf("repositories = %+v", repos)
	}
	h.createRoutine("inventory")
	bad := store.Routine{Name: "Bad Name", Mode: "run", Prompt: "x", Model: "haiku", TimeoutSeconds: 300}
	h.call(http.MethodPost, "/api/v1/routines", bad, nil, http.StatusBadRequest)
	bad.Name = "inventory"
	h.call(http.MethodPost, "/api/v1/routines", bad, nil, http.StatusConflict)
	bad.Name, bad.Model = "other", "gpt-x"
	if raw := h.call(http.MethodPost, "/api/v1/routines", bad, nil, http.StatusBadRequest); !bytes.Contains(raw, []byte("unknown model alias")) {
		t.Errorf("unknown model: %s", raw)
	}
	bad.Model, bad.TimeoutSeconds = "haiku", 0
	h.call(http.MethodPost, "/api/v1/routines", bad, nil, http.StatusBadRequest)
	var got store.Routine
	h.call(http.MethodGet, "/api/v1/routines/inventory", nil, &got, http.StatusOK)
	if got.Generation != 1 || got.Executor != "claude-code" {
		t.Errorf("routine = %+v", got)
	}
	got.Prompt = "changed {{repo}}"
	h.call(http.MethodPut, "/api/v1/routines/inventory?generation=1", got, &got, http.StatusOK)
	h.call(http.MethodPut, "/api/v1/routines/inventory?generation=1", got, nil, http.StatusConflict)
	h.call(http.MethodPut, "/api/v1/routines/inventory", got, nil, http.StatusBadRequest)
	if got.Generation != 2 {
		t.Errorf("generation after update = %d", got.Generation)
	}
	var list []store.Routine
	h.call(http.MethodGet, "/api/v1/routines", nil, &list, http.StatusOK)
	if len(list) != 1 {
		t.Errorf("list = %+v", list)
	}
	h.call(http.MethodDelete, "/api/v1/routines/inventory", nil, nil, http.StatusNoContent)
	h.call(http.MethodPost, "/api/v1/routines/inventory/run", nil, nil, http.StatusConflict)
	h.call(http.MethodGet, "/api/v1/routines/missing", nil, nil, http.StatusNotFound)
}

func TestClaimThroughCompletion(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	created := h.run("inventory")
	if status, _ := h.claim(otherWorker, "r0"); status != http.StatusNotFound {
		t.Errorf("unregistered claim = %d", status)
	}
	c := h.mustClaim("r1")
	// The prompt is the routine prompt with {{repo}} substituted, wrapped by the
	// autonomy block and context line (renderPrompt).
	if c.TargetID != created.Targets[0].ID || !strings.Contains(c.Prompt, "list files in equitizr") || !strings.Contains(c.Prompt, "AUTONOMY:") || c.Model != "claude-haiku-4-5-20251001" || c.MCPToken == "" || len(c.MCPToken) != 64 {
		t.Errorf("claim = %+v", c)
	}
	if !c.LeaseExpiresAt.Equal(h.clock.Now().Add(store.LeaseDuration)) || !c.Policy.RequireSandbox || c.Policy.AllowHosts[0] != "api.anthropic.com" || c.Policy.GitConfig["merge.conflictstyle"] != "zdiff3" {
		t.Errorf("claim lease/policy = %+v %+v", c.LeaseExpiresAt, c.Policy)
	}
	if c.TimeoutSeconds != 300 || c.Executor != "claude-code" || c.Autonomy != model.AutonomyCheckpoint || c.RoutineName != "inventory" || c.Generation != 1 {
		t.Errorf("claim snapshot fields = %+v", c)
	}
	if status, _ := h.claim(testWorkerID, "r2"); status != http.StatusNoContent {
		t.Errorf("second claim = %d", status)
	}
	if status, again := h.claim(testWorkerID, "r1"); status != http.StatusOK || again.AttemptID != c.AttemptID {
		t.Errorf("replayed claim = %d %+v", status, again)
	}
	h.heartbeat(c, model.Preparing, 0)
	hb := h.heartbeat(c, model.Running, 4242)
	if tg := h.target(c.TargetID); tg.State != model.Running || hb.LeaseExpiresAt.Before(h.clock.Now().Add(store.LeaseDuration)) || hb.LogLevels != "info" {
		t.Errorf("after heartbeats: %+v %+v", tg, hb)
	}
	if a := h.attempt(c.AttemptID); a.PID != 4242 || a.Launches != 1 || a.SessionID != "sess-1" {
		t.Errorf("attempt after heartbeat = %+v", a)
	}
	now := h.clock.Now()
	batch := protocol.EventBatch{Source: protocol.SourceWorker, Events: []protocol.Event{
		{Seq: 0, Time: now, Kind: protocol.KindSpanStart, Name: "agent-1", SpanID: "agent-1"},
		{Seq: 1, Time: now, ElapsedUS: 500, Kind: protocol.KindSpanEnd, Name: "agent-1", SpanID: "agent-1", DurationUS: 500},
		{Seq: 2, Time: now, Kind: protocol.KindMetric, Name: "rate_limit", Attrs: json.RawMessage(`{"five_hour_utilization":0.42,"five_hour_resets_at":1756580400,"seven_day_utilization":0.1,"seven_day_resets_at":1756900000}`)},
	}}
	var ins map[string]int
	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/events", batch, &ins, http.StatusOK)
	if ins["inserted"] != 3 {
		t.Errorf("inserted = %v", ins)
	}
	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/events", batch, &ins, http.StatusOK)
	if ins["inserted"] != 0 {
		t.Errorf("resend inserted = %v", ins)
	}
	samples, err := h.st.SamplesSince(context.Background(), "five_hour", time.Time{})
	if err != nil || len(samples) != 1 || samples[0].Utilization != 0.42 || samples[0].SourceAttempt != c.AttemptID || samples[0].ResetsAt.Unix() != 1756580400 {
		t.Errorf("samples = %+v %v", samples, err)
	}
	batch.Source = "nowhere"
	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/events", batch, nil, http.StatusBadRequest)
	var events []store.StoredEvent
	h.call(http.MethodGet, "/api/v1/attempts/"+c.AttemptID+"/events?lines=false", nil, &events, http.StatusOK)
	if len(events) != 4 || events[0].Source != protocol.SourceControl || events[0].Name != "queue_wait" {
		t.Errorf("events = %+v", events)
	}
	done := h.complete(c, completeRequest(model.Succeeded, now))
	if done.State != model.Succeeded || done.Late {
		t.Errorf("complete = %+v", done)
	}
	var detail attemptDetail
	h.call(http.MethodGet, "/api/v1/attempts/"+c.AttemptID, nil, &detail, http.StatusOK)
	if detail.Facts == nil || detail.Facts.InputTokens == nil || *detail.Facts.InputTokens != 10 || *detail.Facts.OutputTokens != 20 || detail.Facts.Project != "default" || detail.Facts.Phases["agent"] == nil || *detail.Facts.Phases["agent"] != 500 || detail.Facts.FiveHourAfter == nil {
		t.Errorf("facts = %+v", detail.Facts)
	}
	var wd workDetail
	h.call(http.MethodGet, "/api/v1/work/"+c.WorkID, nil, &wd, http.StatusOK)
	if wd.State != model.WorkSucceeded || len(wd.Attempts) != 1 || wd.Work.FinishedAt.IsZero() {
		t.Errorf("work detail = %+v", wd)
	}
	var ws workerAttemptState
	h.call(http.MethodGet, "/api/v1/worker/attempts/"+c.AttemptID, nil, &ws, http.StatusOK)
	if !ws.Terminal || ws.Resumable || ws.TargetState != model.Succeeded {
		t.Errorf("worker attempt state = %+v", ws)
	}
	done = h.complete(c, completeRequest(model.Succeeded, now))
	h.call(http.MethodPatch, "/api/v1/attempts/"+c.AttemptID+"/cleanup", protocol.CleanupPatch{Cleanup: protocol.Cleanup{Outcome: "retained", Reason: "kept"}}, nil, http.StatusNoContent)
	if tg := h.target(c.TargetID); !tg.Retained {
		t.Errorf("retained not set: %+v", tg)
	}
}

func TestLateCompletionAfterSweep(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	h.run("inventory")
	c := h.mustClaim("r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 99)
	h.clock.Advance(store.LeaseDuration + time.Second)
	h.srv.sweep(context.Background())
	tg := h.target(c.TargetID)
	if tg.State != model.Failed || tg.FailureReason != model.ReasonLeaseExpired {
		t.Fatalf("after sweep: %+v", tg)
	}
	facts, err := h.st.FactsForAttempt(context.Background(), c.AttemptID)
	if err != nil || facts.State != model.Failed || facts.FailureReason != model.ReasonLeaseExpired {
		t.Errorf("facts after sweep = %+v %v", facts, err)
	}
	done := h.complete(c, completeRequest(model.Succeeded, h.clock.Now()))
	if !done.Late || done.State != model.Failed {
		t.Errorf("late complete = %+v", done)
	}
	if a := h.attempt(c.AttemptID); a.Cleanup.Outcome != "removed" || a.Git.Commits != 1 || a.FailureReason != model.ReasonLeaseExpired {
		t.Errorf("attempt after late completion = %+v", a)
	}
	entries, err := h.st.JournalForEntity(context.Background(), store.EntityAttempt, c.AttemptID)
	if err != nil || !hasKind(entries, "attempt.late_completion") {
		t.Errorf("journal = %+v %v", entries, err)
	}
}

func hasKind(entries []store.JournalEntry, kind string) bool {
	for _, e := range entries {
		if e.Kind == kind {
			return true
		}
	}
	return false
}

func TestWaitingHumanAndResume(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	h.run("inventory")
	c := h.mustClaim("r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 1)
	req := completeRequest(model.WaitingHuman, h.clock.Now())
	req.Question = &protocol.QuestionRequest{Text: "which branch?", Options: []string{"main", "dev"}}
	done := h.complete(c, req)
	if done.State != model.WaitingHuman {
		t.Fatalf("complete = %+v", done)
	}
	var att attention
	h.call(http.MethodGet, "/api/v1/attention", nil, &att, http.StatusOK)
	if len(att.Questions) != 1 || att.Questions[0].AttemptID != c.AttemptID || att.Questions[0].Text != "which branch?" {
		t.Fatalf("attention = %+v", att)
	}
	var ws workerAttemptState
	h.call(http.MethodGet, "/api/v1/worker/attempts/"+c.AttemptID, nil, &ws, http.StatusOK)
	if ws.Terminal || !ws.Resumable {
		t.Errorf("worker attempt state = %+v", ws)
	}
	h.clock.Advance(time.Minute)
	var q store.Question
	h.call(http.MethodPost, "/api/v1/questions/"+att.Questions[0].ID+"/answer", answerRequest{Answer: "main"}, &q, http.StatusOK)
	if q.Answer != "main" || q.AnsweredBy != "human" {
		t.Errorf("answered = %+v", q)
	}
	if tg := h.target(c.TargetID); tg.State != model.Pending || tg.WorkerID != testWorkerID {
		t.Errorf("after answer: %+v", tg)
	}
	if status, _ := h.claim(otherWorker, "rx"); status != http.StatusNotFound {
		t.Errorf("other worker claim = %d", status)
	}
	resumed := h.mustClaim("r2")
	if resumed.AttemptID != c.AttemptID || resumed.Resume == nil || resumed.Resume.SessionID != "sess-1" || resumed.Resume.Answer != "main" || resumed.Resume.Launches != 1 {
		t.Errorf("resume = %+v", resumed)
	}
	var events []store.StoredEvent
	h.call(http.MethodGet, "/api/v1/attempts/"+c.AttemptID+"/events?lines=false", nil, &events, http.StatusOK)
	var waits []int64
	for _, e := range events {
		if e.Name == "queue_wait" {
			waits = append(waits, e.DurationUS)
		}
	}
	if len(waits) != 2 || waits[1] != 0 {
		t.Errorf("queue_wait spans = %v", waits)
	}
}

func TestTasksQueueAndDependencies(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	var first workCreated
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "fix the build\nmore detail", Repositories: []string{"equitizr"}}, &first, http.StatusCreated)
	if first.Work.RoutineName != adHocRoutineName || first.Work.Priority != adHocPriority || first.Work.BudgetClass != model.ClassInteractive || first.Work.Title != "fix the build" || first.Work.Autonomy != model.AutonomyCheckpoint {
		t.Errorf("ad-hoc work = %+v", first.Work)
	}
	snap, err := snapshotRoutine(first.Work)
	if err != nil || snap.Model != adHocModel || snap.TimeoutSeconds != adHocTimeout || snap.MaxTurns != adHocMaxTurns || snap.Mode != adHocMode {
		t.Errorf("snapshot = %+v %v", snap, err)
	}
	var wd workDetail
	h.call(http.MethodGet, "/api/v1/tasks/"+first.Work.ID, nil, &wd, http.StatusOK)
	if wd.State != model.WorkPending || len(wd.Targets) != 1 {
		t.Errorf("task detail = %+v", wd)
	}
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "x", Repositories: []string{"nowhere"}}, nil, http.StatusNotFound)
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Repositories: []string{"equitizr"}}, nil, http.StatusBadRequest)
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "x", Repositories: []string{"equitizr"}, Model: "nope"}, nil, http.StatusBadRequest)
	var second workCreated
	prio := 5
	h.call(http.MethodPost, "/api/v1/work", workRequest{Prompt: "second", Repositories: []string{"equitizr"}, After: []string{first.Work.ID}, Priority: &prio, Class: model.ClassBacklog, Autonomy: model.AutonomyAuto}, &second, http.StatusCreated)
	if second.Work.Priority != 5 || second.Work.BudgetClass != model.ClassBacklog || second.Work.Autonomy != model.AutonomyAuto {
		t.Errorf("second = %+v", second.Work)
	}
	var queue []queueItem
	h.call(http.MethodGet, "/api/v1/queue", nil, &queue, http.StatusOK)
	if len(queue) != 2 || queue[0].Work.ID != first.Work.ID || queue[1].State != model.WorkBlocked || queue[1].Reason != "waiting_on_dependencies" || queue[1].Waiting[0] != first.Work.ID || queue[1].Position != 1 {
		t.Errorf("queue = %+v", queue)
	}
	h.call(http.MethodPatch, "/api/v1/work/"+first.Work.ID, workPatch{AddBlockedBy: []dependency{{WorkID: second.Work.ID}}}, nil, http.StatusConflict)
	newPrio := 500
	h.call(http.MethodPatch, "/api/v1/work/"+second.Work.ID, workPatch{Priority: &newPrio, RemoveBlockedBy: []string{first.Work.ID}}, &wd, http.StatusOK)
	if wd.Work.Priority != 500 {
		t.Errorf("patched = %+v", wd.Work)
	}
	h.call(http.MethodPatch, "/api/v1/work/"+second.Work.ID, workPatch{AddBlockedBy: []dependency{{WorkID: first.Work.ID, On: "terminal"}}}, &wd, http.StatusOK)
	h.call(http.MethodGet, "/api/v1/queue", nil, &queue, http.StatusOK)
	if queue[0].Work.ID != second.Work.ID || queue[0].State != model.WorkBlocked {
		t.Errorf("queue after patch = %+v", queue)
	}
	c := h.mustClaim("r1")
	if c.WorkID != first.Work.ID {
		t.Errorf("claimed %s, want the unblocked first task", c.WorkID)
	}
	h.call(http.MethodDelete, "/api/v1/tasks/"+second.Work.ID, nil, &wd, http.StatusOK)
	if wd.State != model.WorkCancelled || wd.Targets[0].State != model.Cancelled {
		t.Errorf("cancelled = %+v", wd)
	}
	var list []workSummary
	h.call(http.MethodGet, "/api/v1/work?limit=1", nil, &list, http.StatusOK)
	if len(list) != 1 || list[0].Work.ID != second.Work.ID || list[0].State != model.WorkCancelled {
		t.Errorf("list = %+v", list)
	}
	h.call(http.MethodGet, "/api/v1/work?limit=x", nil, nil, http.StatusBadRequest)
	h.call(http.MethodGet, "/api/v1/work/not-an-id", nil, nil, http.StatusBadRequest)
}

func TestTCPAuth(t *testing.T) {
	h := newHarness(t, transportTCP)
	req := protocol.RegisterRequest{WorkerID: testWorkerID, Name: "laptop", Version: "test", MaxConcurrent: 1}
	if status, raw := h.do(http.MethodPost, "/api/v1/worker/register", req, nil, ""); status != http.StatusUnauthorized || !bytes.Contains(raw, []byte("token required")) {
		t.Errorf("no token = %d %s", status, raw)
	}
	if status, _ := h.do(http.MethodPost, "/api/v1/worker/register", req, nil, "wrong"); status != http.StatusUnauthorized {
		t.Errorf("wrong token = %d", status)
	}
	if status, _ := h.do(http.MethodPost, "/api/v1/worker/register", req, nil, testToken); status != http.StatusOK {
		t.Errorf("with token = %d", status)
	}
	if status, _ := h.do(http.MethodPost, "/api/v1/attempts/"+testWorkerID+"/heartbeat", protocol.HeartbeatRequest{LeaseToken: "x"}, nil, ""); status != http.StatusUnauthorized {
		t.Errorf("heartbeat without token = %d", status)
	}
	if status, _ := h.do(http.MethodGet, "/api/v1/routines", nil, nil, ""); status != http.StatusOK {
		t.Errorf("operator route over tcp = %d", status)
	}
	if status, _ := h.do(http.MethodGet, "/api/v1/attempts/"+testWorkerID+"/events", nil, nil, ""); status != http.StatusOK {
		t.Errorf("operator events read over tcp = %d", status)
	}
}

func TestDraining(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	h.run("inventory")
	c := h.mustClaim("r1")
	h.run("inventory")
	var state map[string]string
	h.call(http.MethodPost, "/api/v1/daemon/drain", nil, &state, http.StatusOK)
	if state["state"] != "draining" || !h.srv.Draining() {
		t.Errorf("drain = %v", state)
	}
	if status, _ := h.claim(testWorkerID, "r2"); status != http.StatusServiceUnavailable {
		t.Errorf("claim while draining = %d", status)
	}
	h.heartbeat(c, model.Preparing, 0)
	h.call(http.MethodPost, "/api/v1/routines/inventory/run", nil, nil, http.StatusServiceUnavailable)
	var hs protocol.Handshake
	h.call(http.MethodGet, "/api/v1/handshake", nil, &hs, http.StatusOK)
	if hs.State != "draining" {
		t.Errorf("handshake = %+v", hs)
	}
	var journal []store.JournalEntry
	h.call(http.MethodGet, "/api/v1/journal?since=0&limit=100", nil, &journal, http.StatusOK)
	if !hasKind(journal, "daemon.draining") {
		t.Errorf("journal lacks daemon.draining: %d entries", len(journal))
	}
	h.heartbeat(c, model.Running, 1)
	if done := h.complete(c, completeRequest(model.Succeeded, h.clock.Now())); done.State != model.Succeeded {
		t.Errorf("complete while draining = %+v", done)
	}
	h.srv.SetDraining(false)
	if status, _ := h.claim(testWorkerID, "r3"); status != http.StatusOK {
		t.Errorf("claim after drain lifted = %d", status)
	}
}

func TestServeShutsDownOnCancel(t *testing.T) {
	h := newHarness(t, "")
	ctx, cancel := context.WithCancel(context.Background())
	l := httptest.NewUnstartedServer(nil).Listener
	done := make(chan error, 1)
	go func() { done <- h.srv.Serve(ctx, nil, l) }()
	resp, err := http.Get(fmt.Sprintf("http://%s/api/v1/handshake", l.Addr()))
	if err != nil {
		t.Fatal(err)
	}
	if err := resp.Body.Close(); err != nil {
		t.Fatal(err)
	}
	if resp.StatusCode != http.StatusOK {
		t.Errorf("handshake over Serve = %d", resp.StatusCode)
	}
	resp, err = http.Post(fmt.Sprintf("http://%s/api/v1/worker/claim", l.Addr()), "application/json", bytes.NewReader([]byte(`{}`)))
	if err != nil {
		t.Fatal(err)
	}
	if err := resp.Body.Close(); err != nil {
		t.Fatal(err)
	}
	if resp.StatusCode != http.StatusUnauthorized {
		t.Errorf("worker route over a real tcp listener = %d, want 401", resp.StatusCode)
	}
	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Errorf("Serve = %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Serve did not return after cancel")
	}
}
