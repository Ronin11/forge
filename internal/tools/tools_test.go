package tools_test

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
	"forge/internal/tools"
)

const workerID = "0123456789abcdef0123456789abcdef"

func ctx() context.Context { return context.Background() }

// must returns v or fails hard, so reads in tests never discard an error.
func must[T any](v T, err error) T {
	if err != nil {
		panic(fmt.Sprintf("unexpected error: %v", err))
	}
	return v
}

// obj, arr, str, and num extract decoded JSON values with the type checked,
// so a shape regression fails with a message instead of a panic.
func obj(t *testing.T, v any) map[string]any {
	t.Helper()
	m, ok := v.(map[string]any)
	if !ok {
		t.Fatalf("not a JSON object: %v", v)
	}
	return m
}

func arr(t *testing.T, v any) []any {
	t.Helper()
	a, ok := v.([]any)
	if !ok {
		t.Fatalf("not a JSON array: %v", v)
	}
	return a
}

func str(t *testing.T, v any) string {
	t.Helper()
	s, ok := v.(string)
	if !ok {
		t.Fatalf("not a JSON string: %v", v)
	}
	return s
}

func num(t *testing.T, v any) float64 {
	t.Helper()
	f, ok := v.(float64)
	if !ok {
		t.Fatalf("not a JSON number: %v", v)
	}
	return f
}

// fixture is the minimal daemon-side world (the shape of
// store/lifecycle_test.go): a real store with one project, one worker with one
// repository, one routine, plus a kb dir and the tool registry under test.
type fixture struct {
	t    *testing.T
	s    *store.Store
	deps tools.Deps
	reg  *tools.Registry
	now  time.Time
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	f := &fixture{t: t, now: time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)}
	s, err := store.Open(ctx(), filepath.Join(t.TempDir(), "forge.sqlite3"), store.Options{Clock: func() time.Time { return f.now }})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := s.Close(); err != nil {
			t.Error(err)
		}
	})
	f.s = s
	f.write(func(tx *store.Tx) error {
		if err := tx.EnsureProject(ctx(), "default"); err != nil {
			return err
		}
		if err := tx.Register(ctx(), protocol.RegisterRequest{WorkerID: workerID, Name: "laptop", Version: "test", MaxConcurrent: 2, Executors: []string{"claude-code"},
			Repositories: []protocol.Repository{{Name: "equitizr", Path: "/tmp/equitizr", OriginIdentity: "github.com/x/equitizr"}}}); err != nil {
			return err
		}
		return tx.CreateRoutine(ctx(), &store.Routine{Name: "inventory", Target: "directive:inventory", Repositories: []string{"equitizr"}, TimeoutSeconds: 300})
	})
	f.deps = tools.Deps{Store: s, Write: s.Write, KbDir: t.TempDir(), Clock: func() time.Time { return f.now }, Logger: slog.New(slog.DiscardHandler)}
	f.reg = tools.Defaults()
	return f
}

func (f *fixture) write(fn func(tx *store.Tx) error) {
	f.t.Helper()
	if err := f.s.Write(ctx(), fn); err != nil {
		f.t.Fatal(err)
	}
}

// newWork creates one open Work with one pending Target; the clock advances a
// second so created_at breaks queue-order ties deterministically.
func (f *fixture) newWork(title string, priority int, class model.BudgetClass, edges ...model.Edge) (*store.Work, store.Target) {
	f.t.Helper()
	f.now = f.now.Add(time.Second)
	w := &store.Work{RoutineName: "inventory", Generation: 1, Title: title, Trigger: model.TriggerManual, Snapshot: []byte(`{}`), Priority: priority, BudgetClass: class, Autonomy: model.AutonomyAuto}
	var targets []store.Target
	f.write(func(tx *store.Tx) error {
		var err error
		targets, err = tx.CreateWork(ctx(), w, []string{"equitizr"}, edges)
		return err
	})
	return w, targets[0]
}

// attempt creates a Work, claims its Target for the worker, and heartbeats it
// to running — the state a tool-calling agent is always in.
func (f *fixture) attempt(autonomy model.Autonomy, req string) (*store.Work, store.Target, *store.Attempt) {
	f.t.Helper()
	w, target := f.newWork("inventory "+req, 100, model.ClassNormal)
	var a *store.Attempt
	f.write(func(tx *store.Tx) error {
		var err error
		a, err = tx.Claim(ctx(), store.ClaimParams{TargetID: target.ID, WorkerID: workerID, ClaimRequestID: req, LeaseToken: "lease-" + req, MCPToken: "mcp-" + req,
			Executor: "claude-code", Model: "claude-haiku-4-5", ModelAlias: "haiku", Mode: "run", Autonomy: autonomy})
		return err
	})
	for _, st := range []model.State{model.Preparing, model.Running} {
		f.write(func(tx *store.Tx) error {
			_, err := tx.RecordHeartbeat(ctx(), a.ID, protocol.HeartbeatRequest{LeaseToken: "lease-" + req, State: st})
			return err
		})
	}
	return w, target, a
}

func toolAttempt(w *store.Work, t store.Target, a *store.Attempt) tools.Attempt {
	return tools.Attempt{ID: a.ID, TargetID: t.ID, WorkID: w.ID, Repository: t.Repository, Mode: a.Mode, Autonomy: a.Autonomy, Launches: a.Launches}
}

// call invokes one registered tool and decodes its response, asserting the
// schema_version every response must carry.
func (f *fixture) call(name string, att tools.Attempt, input string) (map[string]any, error) {
	f.t.Helper()
	tool, ok := f.reg.Get(name)
	if !ok {
		f.t.Fatalf("tool %s not registered", name)
	}
	raw, err := tool.Call(ctx(), tools.Request{AttemptID: att.ID, Attempt: att, Input: json.RawMessage(input), Deps: f.deps})
	if err != nil {
		return nil, err
	}
	var out map[string]any
	if err := json.Unmarshal(raw, &out); err != nil {
		f.t.Fatalf("%s: response not JSON: %v", name, err)
	}
	if out["schema_version"] != float64(1) {
		f.t.Errorf("%s: schema_version = %v", name, out["schema_version"])
	}
	return out, nil
}

// mustCall is call for the happy path.
func (f *fixture) mustCall(name string, att tools.Attempt, input string) map[string]any {
	f.t.Helper()
	out, err := f.call(name, att, input)
	if err != nil {
		f.t.Fatalf("%s(%s): %v", name, input, err)
	}
	return out
}

func TestRegistry(t *testing.T) {
	reg := tools.Defaults()
	wantNames := []string{
		"forge_ask", "forge_attempt", "forge_check", "forge_diff_summary", "forge_events",
		"forge_kb_backlinks", "forge_kb_links", "forge_kb_new", "forge_kb_note", "forge_kb_search",
		"forge_note_progress", "forge_prompt_version", "forge_propose", "forge_queue",
		"forge_repo_status", "forge_request_budget", "forge_retro_pack", "forge_stats", "forge_usage",
	}
	all := reg.All()
	names := make([]string, len(all))
	for i, tl := range all {
		names[i] = tl.Name()
		var schema map[string]any
		if err := json.Unmarshal(tl.InputSchema(), &schema); err != nil {
			t.Errorf("%s: schema not JSON: %v", tl.Name(), err)
			continue
		}
		if ap, ok := schema["additionalProperties"].(bool); !ok || ap {
			t.Errorf("%s: schema must set additionalProperties:false", tl.Name())
		}
		local := tl.Name() == "forge_repo_status" || tl.Name() == "forge_check" || tl.Name() == "forge_diff_summary"
		want := tools.WhereDaemon
		if local {
			want = tools.WhereLocal
		}
		if tl.Where() != want {
			t.Errorf("%s: where = %s, want %s", tl.Name(), tl.Where(), want)
		}
		if local {
			if _, err := tl.Call(ctx(), tools.Request{}); err == nil || !strings.Contains(err.Error(), "local tool") {
				t.Errorf("%s: local Call = %v, want a 'local tool' error", tl.Name(), err)
			}
		}
	}
	if fmt.Sprint(names) != fmt.Sprint(wantNames) {
		t.Errorf("All() = %v\nwant     %v", names, wantNames)
	}
	u, _ := reg.Get("forge_usage")
	dup := tools.NewRegistry()
	if err := dup.Register(u); err != nil {
		t.Fatal(err)
	}
	if err := dup.Register(u); err == nil {
		t.Error("duplicate registration accepted")
	}
}

func TestUsage(t *testing.T) {
	f := newFixture(t)
	w, tg, a := f.attempt(model.AutonomyAuto, "r1")
	att := toolAttempt(w, tg, a)
	out := f.mustCall("forge_usage", att, `{}`)
	windows := obj(t, out["windows"])
	if windows["five_hour"] != nil || windows["seven_day"] != nil {
		t.Errorf("windows without samples = %v", windows)
	}
	if !strings.Contains(str(t, out["note"]), "M3") {
		t.Errorf("note = %v", out["note"])
	}
	f.write(func(tx *store.Tx) error {
		return tx.InsertSamples(ctx(), []store.RateLimitSample{
			{Time: f.now, Window: "five_hour", Utilization: 0.4, ResetsAt: f.now.Add(time.Hour), SourceAttempt: a.ID},
			{Time: f.now, Window: "seven_day", Utilization: 0.7, ResetsAt: f.now.Add(6 * 24 * time.Hour), SourceAttempt: a.ID},
		})
	})
	out = f.mustCall("forge_usage", att, `{}`)
	windows = obj(t, out["windows"])
	if u := obj(t, windows["five_hour"])["utilization"]; u != 0.4 {
		t.Errorf("five_hour utilization = %v", u)
	}
	if u := obj(t, windows["seven_day"])["utilization"]; u != 0.7 {
		t.Errorf("seven_day utilization = %v", u)
	}
	if _, err := f.call("forge_usage", att, `{"bogus":1}`); !tools.IsBadInput(err) {
		t.Errorf("unknown field = %v", err)
	}
}

func TestAttempt(t *testing.T) {
	f := newFixture(t)
	w, tg, a := f.attempt(model.AutonomyAuto, "r1")
	att := toolAttempt(w, tg, a)
	out := f.mustCall("forge_attempt", att, `{}`)
	if got := obj(t, out["attempt"])["id"]; got != a.ID {
		t.Errorf("attempt.id = %v", got)
	}
	if st := obj(t, out["target"])["state"]; st != "running" {
		t.Errorf("target.state = %v", st)
	}
	if out["facts"] != nil {
		t.Errorf("facts before terminal = %v", out["facts"])
	}
	if _, err := f.call("forge_attempt", att, `{"attempt_id":"ffffffffffffffffffffffffffffffff"}`); !errors.Is(err, store.ErrNotFound) {
		t.Errorf("unknown attempt = %v", err)
	}
	if _, err := f.call("forge_attempt", att, `{"attempt_id":"nope"}`); !tools.IsBadInput(err) {
		t.Errorf("malformed attempt id = %v", err)
	}
}

func TestEvents(t *testing.T) {
	f := newFixture(t)
	w, tg, a := f.attempt(model.AutonomyAuto, "r1")
	att := toolAttempt(w, tg, a)
	f.write(func(tx *store.Tx) error {
		_, err := tx.InsertEvents(ctx(), a.ID, protocol.SourceWorker, []protocol.Event{
			{Seq: 0, Time: f.now, ElapsedUS: 0, Kind: protocol.KindLifecycle, Message: "claimed"},
			{Seq: 1, Time: f.now, ElapsedUS: 10, Kind: protocol.KindStdout, Message: "line"},
			{Seq: 2, Time: f.now, ElapsedUS: 20, Kind: protocol.KindLifecycle, Message: "done"},
		})
		return err
	})
	out := f.mustCall("forge_events", att, `{}`)
	if n := out["count"]; n != float64(2) {
		t.Errorf("default (no lines) count = %v", n)
	}
	out = f.mustCall("forge_events", att, `{"include_lines":true}`)
	if n := out["count"]; n != float64(3) {
		t.Errorf("with lines count = %v", n)
	}
	out = f.mustCall("forge_events", att, `{"include_lines":true,"limit":1,"offset":1}`)
	events := arr(t, out["events"])
	if len(events) != 1 || obj(t, events[0])["message"] != "line" {
		t.Errorf("page = %v", events)
	}
	if _, err := f.call("forge_events", att, `{"attempt_id":"ffffffffffffffffffffffffffffffff"}`); !errors.Is(err, store.ErrNotFound) {
		t.Errorf("unknown attempt = %v", err)
	}
	if _, err := f.call("forge_events", att, `{"limit":-1}`); !tools.IsBadInput(err) {
		t.Errorf("negative limit = %v", err)
	}
}

func TestPromptVersion(t *testing.T) {
	f := newFixture(t)
	w, tg, a := f.attempt(model.AutonomyAuto, "r1")
	att := toolAttempt(w, tg, a)
	if _, err := f.call("forge_prompt_version", att, `{}`); !tools.IsBadInput(err) {
		t.Errorf("no prompt version yet = %v", err)
	}
	f.write(func(tx *store.Tx) error {
		_, err := tx.RecordHeartbeat(ctx(), a.ID, protocol.HeartbeatRequest{LeaseToken: "lease-r1", State: model.Running,
			PromptVersion: &protocol.PromptVersion{Hash: "h1", Routine: "inventory", Generation: 1, Mode: "run", Model: "haiku"}})
		return err
	})
	out := f.mustCall("forge_prompt_version", att, `{}`)
	if h := obj(t, out["prompt_version"])["hash"]; h != "h1" {
		t.Errorf("hash = %v", h)
	}
	if _, err := f.call("forge_prompt_version", att, `{"hash":"missing"}`); !errors.Is(err, store.ErrNotFound) {
		t.Errorf("unknown hash = %v", err)
	}
}

func TestQueueOrdering(t *testing.T) {
	f := newFixture(t)
	w1, _ := f.newWork("low", 100, model.ClassNormal)
	w2, _ := f.newWork("backlog", 200, model.ClassBacklog)
	w3, _ := f.newWork("interactive", 200, model.ClassInteractive)
	w4, _ := f.newWork("blocked", 300, model.ClassNormal, model.Edge{BlockedBy: w1.ID, On: model.OnSuccess})
	out := f.mustCall("forge_queue", tools.Attempt{}, `{}`)
	var ids, states []string
	for _, r := range arr(t, out["queue"]) {
		row := obj(t, r)
		ids = append(ids, str(t, row["work_id"]))
		states = append(states, str(t, row["state"]))
	}
	// priority desc, then class rank (interactive < backlog), then created_at.
	want := []string{w4.ID, w3.ID, w2.ID, w1.ID}
	if fmt.Sprint(ids) != fmt.Sprint(want) {
		t.Errorf("order = %v, want %v", ids, want)
	}
	if states[0] != "blocked" || states[1] != "pending" {
		t.Errorf("states = %v", states)
	}
	out = f.mustCall("forge_queue", tools.Attempt{}, `{"limit":2,"offset":1}`)
	rows := arr(t, out["queue"])
	if len(rows) != 2 || obj(t, rows[0])["work_id"] != w3.ID {
		t.Errorf("page = %v", rows)
	}
}

// seedFacts inserts three synthetic facts rows for the routine "inventory":
// a verified success, an unverified claim, and a failure with events, at
// distinct finished_at times so newest-first is deterministic.
func seedFacts(f *fixture) (a1, a2, a3 *store.Attempt) {
	f.t.Helper()
	_, _, a1 = f.attempt(model.AutonomyAuto, "s1")
	_, _, a2 = f.attempt(model.AutonomyAuto, "s2")
	_, _, a3 = f.attempt(model.AutonomyAuto, "s3")
	base := f.now
	row := func(a *store.Attempt, st model.State, total, agent, in, out int64, cost *float64, pass, isErr *bool, reason model.FailureReason, age time.Duration) *store.AttemptFacts {
		return &store.AttemptFacts{AttemptID: a.ID, TargetID: a.TargetID, WorkID: "", Routine: "inventory", Generation: 1, Project: "default",
			Repository: "equitizr", Worker: workerID, Executor: "claude-code", Model: "haiku", Mode: "run", Trigger: model.TriggerManual, Autonomy: model.AutonomyAuto,
			Phases: map[string]*int64{"total": &total, "agent": &agent}, FinishedAt: base.Add(-age), State: st, FailureReason: reason,
			InputTokens: &in, OutputTokens: &out, CostUSD: cost, VerificationPass: pass, IsError: isErr}
	}
	cost1, cost2 := 1.0, 2.0
	passed, failedV, selfOK, selfErr := true, false, false, true
	f.write(func(tx *store.Tx) error {
		if err := tx.InsertFacts(ctx(), row(a1, model.Succeeded, 100, 110, 10, 1, &cost1, &passed, &selfOK, "", 3*time.Hour)); err != nil {
			return err
		}
		if err := tx.InsertFacts(ctx(), row(a2, model.Unverified, 200, 220, 20, 2, &cost2, &failedV, &selfOK, "", 2*time.Hour)); err != nil {
			return err
		}
		if err := tx.InsertFacts(ctx(), row(a3, model.Failed, 300, 330, 30, 3, nil, nil, &selfErr, model.ReasonExitNonzero, time.Hour)); err != nil {
			return err
		}
		_, err := tx.InsertEvents(ctx(), a3.ID, protocol.SourceWorker, []protocol.Event{
			{Seq: 0, Time: base, ElapsedUS: 0, Kind: protocol.KindSpanEnd, SpanID: "agent-1", Name: "agent", DurationUS: 330},
			{Seq: 1, Time: base, ElapsedUS: 5, Kind: protocol.KindStdout, Message: "boom"},
		})
		return err
	})
	return a1, a2, a3
}

func TestStatsPercentiles(t *testing.T) {
	f := newFixture(t)
	seedFacts(f)
	att := tools.Attempt{}
	out := f.mustCall("forge_stats", att, `{}`)
	report := obj(t, out["report"])
	routines := arr(t, report["routines"])
	if len(routines) != 1 {
		t.Fatalf("routines = %v, want the inventory rollup only", routines)
	}
	inv := obj(t, routines[0])
	if inv["routine"] != "inventory" || inv["generation"] != float64(0) {
		t.Errorf("rollup row = %v@%v", inv["routine"], inv["generation"])
	}
	for key, want := range map[string]float64{
		"runs": 3, "verified_successes": 1,
		"p50_total_us": 200, "p95_total_us": 300, "max_total_us": 300,
		"p50_agent_us": 220, "p95_agent_us": 330, "max_agent_us": 330,
		"tokens_in_per_run": 20, "tokens_out_per_run": 2,
		"cost_usd_total": 3, "cost_per_run": 1.5, "cost_per_verified_success": 3,
	} {
		if got := inv[key]; got != want {
			t.Errorf("%s = %v, want %v", key, got, want)
		}
	}
	oc := obj(t, inv["outcomes"])
	if oc["succeeded"] != float64(1) || oc["unverified"] != float64(1) || oc["failed"] != float64(1) {
		t.Errorf("outcomes = %v", oc)
	}
	if r := num(t, inv["verified_success_rate"]); r < 0.33 || r > 0.34 {
		t.Errorf("verified_success_rate = %v", r)
	}
	// is_error is recorded on all three rows — false, false, true → 2/3.
	if r := num(t, inv["self_reported_success_rate"]); r < 0.66 || r > 0.67 {
		t.Errorf("self_reported_success_rate = %v", r)
	}
	reasons := arr(t, inv["top_failure_reasons"])
	if len(reasons) != 1 || obj(t, reasons[0])["reason"] != "exit_nonzero" {
		t.Errorf("top_failure_reasons = %v", reasons)
	}
	gens := arr(t, report["generations"])
	if len(gens) != 1 || obj(t, gens[0])["generation"] != float64(1) || obj(t, gens[0])["runs"] != float64(3) {
		t.Errorf("generations = %v", gens)
	}
	if tot := report["total_runs"]; tot != float64(3) {
		t.Errorf("total_runs = %v", tot)
	}
	out = f.mustCall("forge_stats", att, `{"routine":"other"}`)
	if rs := arr(t, obj(t, out["report"])["routines"]); len(rs) != 0 {
		t.Errorf("filtered routines = %v", rs)
	}
	if _, err := f.call("forge_stats", att, `{"since_hours":-1}`); !tools.IsBadInput(err) {
		t.Errorf("negative window = %v", err)
	}
}

func TestRetroPack(t *testing.T) {
	f := newFixture(t)
	_, a2, a3 := seedFacts(f)
	out := f.mustCall("forge_retro_pack", tools.Attempt{}, `{}`)
	statsRoutines := arr(t, obj(t, out["stats"])["routines"])
	if len(statsRoutines) != 1 || obj(t, statsRoutines[0])["runs"] != float64(3) {
		t.Errorf("stats routines = %v", statsRoutines)
	}
	// The current routine settings ride along for the retro reader.
	current := arr(t, out["routines"])
	if len(current) != 1 || obj(t, current[0])["name"] != "inventory" {
		t.Errorf("routines = %v", current)
	}
	problems := arr(t, out["problem_attempts"])
	if len(problems) != 2 {
		t.Fatalf("problem_attempts = %d, want 2", len(problems))
	}
	first := obj(t, problems[0])
	if id := obj(t, first["facts"])["attempt_id"]; id != a3.ID {
		t.Errorf("newest problem = %v, want the failed attempt %s", id, a3.ID)
	}
	if id := obj(t, obj(t, problems[1])["facts"])["attempt_id"]; id != a2.ID {
		t.Errorf("second problem = %v, want the unverified attempt %s", id, a2.ID)
	}
	events := arr(t, first["events"])
	if len(events) != 1 || obj(t, events[0])["name"] != "agent" {
		t.Errorf("problem events = %v (stdout lines must be excluded)", events)
	}
}

func TestKb(t *testing.T) {
	f := newFixture(t)
	w, tg, a := f.attempt(model.AutonomyAuto, "r1")
	att := toolAttempt(w, tg, a)
	out := f.mustCall("forge_kb_new", att, fmt.Sprintf(
		`{"title":"Retro Findings","type":"note","tags":["retro"],"body":"See [[other-note]].","links":{"about":["attempt:%s"]}}`, a.ID))
	if out["id"] != "retro-findings" {
		t.Fatalf("id = %v", out["id"])
	}
	if _, err := os.Stat(str(t, out["path"])); err != nil {
		t.Fatalf("note file: %v", err)
	}
	// Immediately searchable.
	found := f.mustCall("forge_kb_search", att, `{"query":"Findings"}`)
	notes := arr(t, found["notes"])
	if len(notes) != 1 || obj(t, notes[0])["id"] != "retro-findings" {
		t.Errorf("search = %v", notes)
	}
	// A second note must not drop the first from the index (IndexKbNote, not
	// ReindexKb with a partial list).
	f.mustCall("forge_kb_new", att, `{"title":"Second Note"}`)
	if again := f.mustCall("forge_kb_search", att, `{"query":"Findings"}`); len(arr(t, again["notes"])) != 1 {
		t.Errorf("first note vanished from the index after a second kb_new")
	}
	got := f.mustCall("forge_kb_note", att, `{"id":"retro-findings"}`)
	if body := str(t, got["body"]); !strings.Contains(body, "See [[other-note]].") {
		t.Errorf("body = %q", body)
	}
	links := arr(t, f.mustCall("forge_kb_links", att, `{"id":"retro-findings"}`)["links"])
	if len(links) != 2 { // about attempt:<id> + inline other-note
		t.Errorf("links = %v", links)
	}
	back := arr(t, f.mustCall("forge_kb_backlinks", att, fmt.Sprintf(`{"ref":"attempt:%s"}`, a.ID))["backlinks"])
	if len(back) != 1 || obj(t, back[0])["from_id"] != "retro-findings" {
		t.Errorf("backlinks = %v", back)
	}
	if _, err := f.call("forge_kb_new", att, `{"title":"X","type":"bogus"}`); !tools.IsBadInput(err) {
		t.Errorf("bad type = %v", err)
	}
	if _, err := f.call("forge_kb_new", att, `{"body":"no title"}`); !tools.IsBadInput(err) {
		t.Errorf("missing title = %v", err)
	}
	if _, err := f.call("forge_kb_backlinks", att, `{"ref":"attempt:zz"}`); !tools.IsBadInput(err) {
		t.Errorf("malformed ref = %v", err)
	}
	if _, err := f.call("forge_kb_note", att, `{"id":"missing-note"}`); !errors.Is(err, store.ErrNotFound) {
		t.Errorf("unknown note = %v", err)
	}
	if _, err := f.call("forge_kb_search", att, `{}`); !tools.IsBadInput(err) {
		t.Errorf("missing query = %v", err)
	}
}

func TestAsk(t *testing.T) {
	f := newFixture(t)
	wAuto, tAuto, aAuto := f.attempt(model.AutonomyAuto, "auto1")
	_, err := f.call("forge_ask", toolAttempt(wAuto, tAuto, aAuto), `{"question":"Which?"}`)
	if !tools.IsBadInput(err) || !strings.Contains(err.Error(), "does not allow questions") {
		t.Errorf("ask at auto = %v", err)
	}
	wAsk, tAsk, aAsk := f.attempt(model.AutonomyAsk, "ask1")
	out := f.mustCall("forge_ask", toolAttempt(wAsk, tAsk, aAsk), `{"question":"Which README?","options":["a","b"],"checkpoint":"after_spec"}`)
	qid := str(t, out["question_id"])
	if qid == "" || !strings.Contains(str(t, out["instruction"]), "end your turn") {
		t.Fatalf("ask = %v", out)
	}
	open := must(f.s.OpenQuestions(ctx()))
	if len(open) != 1 || open[0].ID != qid || open[0].AttemptID != aAsk.ID || open[0].Checkpoint != "after_spec" {
		t.Errorf("open questions = %+v", open)
	}
	// Non-blocking: CreateQuestion alone must not transition the Target; the
	// move to waiting_human happens at completion with an open Question.
	if tg := must(f.s.GetTarget(ctx(), tAsk.ID)); tg.State != model.Running {
		t.Errorf("target after ask = %s, want running", tg.State)
	}
	if _, err := f.call("forge_ask", toolAttempt(wAsk, tAsk, aAsk), `{}`); !tools.IsBadInput(err) {
		t.Errorf("missing question = %v", err)
	}
}

func TestNoteProgress(t *testing.T) {
	f := newFixture(t)
	w, tg, a := f.attempt(model.AutonomyAuto, "r1")
	att := toolAttempt(w, tg, a)
	out := f.mustCall("forge_note_progress", att, `{"message":"halfway","checkpoint":"after_plan"}`)
	if out["ok"] != true {
		t.Errorf("ok = %v", out["ok"])
	}
	f.mustCall("forge_note_progress", att, `{"message":"done"}`)
	var control []store.StoredEvent
	for _, e := range must(f.s.Events(ctx(), a.ID, true, 0)) {
		if e.Source == protocol.SourceControl {
			control = append(control, e)
		}
	}
	if len(control) != 2 {
		t.Fatalf("control events = %d", len(control))
	}
	for i, want := range []string{"halfway", "done"} {
		e := control[i]
		if e.Seq != i || e.Kind != protocol.KindLifecycle || e.Name != "progress" || e.Message != want {
			t.Errorf("event %d = %+v", i, e)
		}
	}
	if !strings.Contains(string(control[0].Attrs), "after_plan") {
		t.Errorf("attrs = %s", control[0].Attrs)
	}
	if _, err := f.call("forge_note_progress", att, `{}`); !tools.IsBadInput(err) {
		t.Errorf("missing message = %v", err)
	}
}

func TestRebindMCPToken(t *testing.T) {
	f := newFixture(t)
	_, _, a := f.attempt(model.AutonomyAuto, "reb")
	if ok := must(f.s.CheckMCPToken(ctx(), a.ID, "mcp-reb")); !ok {
		t.Fatal("original mcp token rejected")
	}
	f.write(func(tx *store.Tx) error { return tx.RebindMCPToken(ctx(), a.ID, "fresh-token") })
	if ok := must(f.s.CheckMCPToken(ctx(), a.ID, "fresh-token")); !ok {
		t.Error("rebound token rejected")
	}
	if ok := must(f.s.CheckMCPToken(ctx(), a.ID, "mcp-reb")); ok {
		t.Error("stale token still accepted")
	}
	err := f.s.Write(ctx(), func(tx *store.Tx) error {
		return tx.RebindMCPToken(ctx(), "ffffffffffffffffffffffffffffffff", "x")
	})
	if !errors.Is(err, store.ErrNotFound) {
		t.Errorf("rebind unknown attempt = %v", err)
	}
	hist := must(f.s.JournalForEntity(ctx(), store.EntityAttempt, a.ID))
	found := false
	for _, h := range hist {
		if h.Kind == "attempt.mcp_rebound" {
			found = true
		}
	}
	if !found {
		t.Error("no attempt.mcp_rebound journal row")
	}
}

// A repo-scoped kb_new lands the note in the repository's own .forge/notes —
// versioned with the code it is about — and indexes it immediately like any
// other note. An unregistered repo is the caller's mistake.
func TestKbNewRepoScoped(t *testing.T) {
	f := newFixture(t)
	repoPath := t.TempDir()
	f.write(func(tx *store.Tx) error {
		return tx.Register(ctx(), protocol.RegisterRequest{WorkerID: "fedcba9876543210fedcba9876543210", Name: "second", Version: "test", MaxConcurrent: 1, Executors: []string{"claude-code"},
			Repositories: []protocol.Repository{{Name: "scoped", Path: repoPath, OriginIdentity: "github.com/x/scoped"}}})
	})
	w, tg, a := f.attempt(model.AutonomyAuto, "kbrepo-1")
	att := toolAttempt(w, tg, a)
	out := f.mustCall("forge_kb_new", att, `{"title":"With Deps Gotcha","repo":"scoped","body":"symlink races the checks"}`)
	path := str(t, out["path"])
	if !strings.HasPrefix(path, filepath.Join(repoPath, ".forge", "notes")) {
		t.Fatalf("note path = %q, want under the repo's .forge/notes", path)
	}
	if _, err := os.Stat(path); err != nil {
		t.Fatalf("note file: %v", err)
	}
	found := f.mustCall("forge_kb_search", att, `{"query":"Gotcha"}`)
	if notes := arr(t, found["notes"]); len(notes) != 1 || obj(t, notes[0])["id"] != "with-deps-gotcha" {
		t.Errorf("search = %v", notes)
	}
	if _, err := f.call("forge_kb_new", att, `{"title":"X","repo":"ghost"}`); !tools.IsBadInput(err) {
		t.Errorf("unregistered repo = %v", err)
	}
}
