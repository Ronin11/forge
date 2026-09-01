package controlplane

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/protocol"
	"forge/internal/store"
)

// routingConfig builds a config with kimi (devbox, cheaper) and haiku (claude),
// so an unproven-but-cheaper kimi routes first and gates out after failures.
func routingConfig(t *testing.T) *Config {
	return loadConfigFrom(t, `
[runners.devbox]
kind = "openai-compatible"
billing = "api"
capacity = 1
endpoint = "http://127.0.0.1:9/v1"

[models.kimi]
runner = "devbox"
id = "kimi-k2"
class = "mid"
max_tier = 3
price = { input = 0.10, output = 0.30 }

[routing]
min_samples = 3
explore = 0.0
min_verified_success = 0.6
`)
}

func newRoutingHarness(t *testing.T) *harness {
	t.Helper()
	clock := &fakeClock{now: time.Date(2026, 8, 31, 12, 0, 0, 0, time.UTC)}
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
	cfg := routingConfig(t)
	levels := "info"
	srv, err := NewServer(ServerOptions{
		Store: st, Clock: clock.Now, Version: "test", Token: testToken, TransportOverride: transportUnix,
		ModelInfo: cfg.ModelInfoFor, ModelAliases: cfg.ModelAliases(), Routing: cfg.Routing, RunnerCapacities: runnerCapacitiesForTest(cfg),
		LogLevels: func() string { return levels }, SetLogLevels: func(spec string) error { levels = spec; return nil },
	})
	if err != nil {
		t.Fatal(err)
	}
	hs := httptest.NewServer(srv.Handler())
	t.Cleanup(hs.Close)
	return &harness{t: t, st: st, srv: srv, http: hs, clock: clock, leases: map[string]string{}}
}

func runnerCapacitiesForTest(cfg *Config) map[string]int {
	out := map[string]int{}
	for name, r := range cfg.Runners {
		if r.Capacity > 0 {
			out[name] = r.Capacity
		}
	}
	return out
}

// registerRunners registers the worker advertising both runners ready.
func (h *harness) registerRunners(workerID string) {
	h.t.Helper()
	req := protocol.RegisterRequest{WorkerID: workerID, Name: "laptop", Version: "test", MaxConcurrent: 4,
		Executors: []string{"claude-code"}, Capabilities: map[string]string{"sandbox": "ready", "runner:claude": "ready", "runner:devbox": "ready"},
		Repositories: []protocol.Repository{{Name: "equitizr", Path: "/tmp/equitizr", OriginIdentity: "github.com/x/equitizr", Project: "default"}}}
	var resp protocol.RegisterResponse
	h.call(http.MethodPost, "/api/v1/worker/register", req, &resp, http.StatusOK)
}

func (h *harness) createLadderRoutine(name string) {
	h.t.Helper()
	tier := 0
	r := store.Routine{Name: name, Mode: "run", Prompt: "do {{repo}}", Repositories: []string{"equitizr"}, Model: "haiku",
		Models: []string{"kimi", "haiku"}, Tier: &tier, TimeoutSeconds: 300, RequireSandbox: true}
	h.call(http.MethodPost, "/api/v1/routines", r, nil, http.StatusCreated)
}

// The router picks the cheaper unproven kimi first; a verification failure then
// escalates the retry to the next ladder rung, haiku, with escalated_from set.
func TestRouteClaimEscalationLadder(t *testing.T) {
	h := newRoutingHarness(t)
	h.registerRunners(testWorkerID)
	h.createLadderRoutine("router")
	work := h.run("router")
	target := work.Targets[0].ID

	c1 := h.mustClaim("c1")
	if c1.Model != "kimi-k2" {
		t.Fatalf("first attempt should route to kimi, got %q", c1.Model)
	}
	a1, err := h.st.AttemptForTarget(context.Background(), target)
	if err != nil {
		t.Fatal(err)
	}
	if a1.ModelAlias != "kimi" || a1.Runner != "devbox" || len(a1.Routing) == 0 {
		t.Errorf("attempt 1 routing record: alias=%q runner=%q routing=%s", a1.ModelAlias, a1.Runner, a1.Routing)
	}

	// Fail verification, then retry: the next claim escalates to haiku.
	h.heartbeat(c1, model.Preparing, 0)
	h.heartbeat(c1, model.Running, 4321)
	req := completeRequest(model.Failed, h.clock.now)
	req.Verification = protocol.Verification{Level: 1, Passed: false}
	h.complete(c1, req)
	fin, err := h.st.AttemptForTarget(context.Background(), target)
	if err != nil {
		t.Fatal(err)
	}
	if fin.FinishedAt.IsZero() {
		t.Fatalf("first attempt not finished after complete: %+v", fin)
	}
	h.call(http.MethodPost, "/api/v1/targets/"+target+"/retry", nil, nil, http.StatusOK)

	c2 := h.mustClaim("c2")
	if c2.Model != "claude-haiku-4-5-20251001" {
		t.Fatalf("escalated attempt should route to haiku, got %q", c2.Model)
	}
	// Read the escalated attempt by its own id (a frozen test clock ties
	// created_at, so "latest attempt" is ambiguous — the claim id is not).
	a2, err := h.st.GetAttempt(context.Background(), c2.AttemptID)
	if err != nil {
		t.Fatal(err)
	}
	if a2.ModelAlias != "haiku" || a2.EscalatedFrom != "kimi" {
		t.Errorf("attempt 2 should be haiku escalated from kimi: alias=%q escalated_from=%q", a2.ModelAlias, a2.EscalatedFrom)
	}
	// The escalation note (previous failure) is injected into the rendered prompt.
	if c2.Prompt == "" || !containsAll(c2.Prompt, "ESCALATION", "kimi") {
		t.Errorf("escalated prompt should carry the prior failure: %q", c2.Prompt)
	}
	// And the template (hashable) does NOT carry the per-attempt escalation note.
	if c2.PromptTemplate != "" && containsAll(c2.PromptTemplate, "ESCALATION") {
		t.Errorf("escalation note leaked into the hashable template: %q", c2.PromptTemplate)
	}
}

func containsAll(s string, subs ...string) bool {
	for _, sub := range subs {
		found := false
		for i := 0; i+len(sub) <= len(s); i++ {
			if s[i:i+len(sub)] == sub {
				found = true
				break
			}
		}
		if !found {
			return false
		}
	}
	return true
}

// createKimiRoutine is a single-model kimi routine: no fallback rung, so a
// full devbox runner has no alternative and the second target must wait.
func (h *harness) createKimiRoutine(name string) {
	h.t.Helper()
	tier := 0
	r := store.Routine{Name: name, Mode: "run", Prompt: "do {{repo}}", Repositories: []string{"equitizr"}, Model: "kimi",
		Models: []string{"kimi"}, Tier: &tier, TimeoutSeconds: 300, Concurrency: 5, RequireSandbox: true}
	h.call(http.MethodPost, "/api/v1/routines", r, nil, http.StatusCreated)
}

// With devbox capacity 1 and two kimi-only targets, the runner lease serialises
// them: the first claims, the second finds the runner full and gets nothing
// (204) even though a worker slot is free.
func TestRouteClaimRunnerCapacity(t *testing.T) {
	h := newRoutingHarness(t)
	h.registerRunners(testWorkerID)
	h.createKimiRoutine("kimionly")
	h.run("kimionly")
	h.run("kimionly")

	c1 := h.mustClaim("cap1")
	if c1.Model != "kimi-k2" {
		t.Fatalf("first claim should be kimi, got %q", c1.Model)
	}
	// devbox now has 1 in flight (capacity 1); the second kimi target cannot run.
	if status, c := h.claim(testWorkerID, "cap2"); status != http.StatusNoContent {
		t.Fatalf("second kimi claim should serialise on the full runner (204), got %d model=%q", status, c.Model)
	}

	// Finish the first attempt: the runner frees and the second target claims.
	h.heartbeat(c1, model.Preparing, 0)
	h.heartbeat(c1, model.Running, 11)
	h.complete(c1, completeRequest(model.Succeeded, h.clock.now))
	c3 := h.mustClaim("cap3")
	if c3.Model != "kimi-k2" {
		t.Fatalf("after the runner frees, the second target claims kimi, got %q", c3.Model)
	}
}
