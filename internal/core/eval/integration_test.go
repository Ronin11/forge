//go:build integration

// Integration coverage for the whole loop: a real daemon, the worker child it
// spawns, and the SQLite store, driven only through the HTTP-on-unix-socket
// API with the fake-claude executor. `just check` already runs the golden eval
// cases (TestRunGoldenCases), which drive the same harness through the CLI and
// score the outcome; these tests instead submit over the API the way a plugin
// or the UI does, and assert the lifecycle rows the store keeps for each of
// the three terminal shapes a task can take: verified success, a verification
// failure, and a question answered by a human.
//
// They reuse this package's harness (makeGitBasic, writeCaseConfig, caseEnv,
// startDaemon) rather than growing a second one. They are behind the
// `integration` build tag and run from `just test-integration`, because each
// case starts a daemon and a worker; `just check` stays as fast as it is.
package eval

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// integrationTimeout bounds one case: daemon start, worker registration, the
// fixture replay, and every poll after it.
const integrationTimeout = 120 * time.Second

// intAttempt is the attempt row an API client sees.
type intAttempt struct {
	ID                 string          `json:"id"`
	SessionID          string          `json:"session_id"`
	Launches           int             `json:"launches"`
	NumTurns           int             `json:"num_turns"`
	Result             json.RawMessage `json:"result"`
	VerificationLevel  *int            `json:"verification_level"`
	VerificationPassed *bool           `json:"verification_passed"`
	Git                struct {
		Commits int `json:"commits"`
	} `json:"git"`
	Cleanup struct {
		Outcome string `json:"outcome"`
		Reason  string `json:"reason"`
	} `json:"cleanup"`
}

// intQuestion is one row of the task's questions[].
type intQuestion struct {
	ID         string    `json:"id"`
	Text       string    `json:"text"`
	Options    []string  `json:"options"`
	Answer     string    `json:"answer"`
	AnsweredBy string    `json:"answered_by"`
	AnsweredAt time.Time `json:"answered_at"`
}

// intTask is the slice of GET /api/v1/tasks/{id} these tests assert on. It is
// deliberately its own view rather than store.Work et al: the daemon's JSON is
// the contract an API client sees, and a field rename should fail here.
type intTask struct {
	Work struct {
		ID string `json:"id"`
	} `json:"work"`
	State   string `json:"state"`
	Targets []struct {
		ID               string `json:"id"`
		State            string `json:"state"`
		FailureReason    string `json:"failure_reason"`
		UnverifiedReason string `json:"unverified_reason"`
		Retained         bool   `json:"retained"`
	} `json:"targets"`
	Attempts  []intAttempt  `json:"attempts"`
	Questions []intQuestion `json:"questions"`
}

func (v *intTask) attempt(t *testing.T) *intAttempt {
	t.Helper()
	if len(v.Attempts) == 0 {
		t.Fatalf("task %s has no attempts", v.Work.ID)
	}
	return &v.Attempts[len(v.Attempts)-1]
}

// intHarness is one case's isolated world: a temporary FORGE_HOME, a generated
// git repository, a daemon child, and an HTTP client bound to its socket.
type intHarness struct {
	t    *testing.T
	ctx  context.Context
	home string
	repo string
	env  []string
	http *http.Client
}

// startIntegration builds the case home, pins the fake-claude fixture in the
// daemon's environment, optionally commits a forge.toml into the repository
// (the declared checks L1 verification runs), and starts the daemon.
func startIntegration(t *testing.T, fixtureName, repoForgeToml string) *intHarness {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), integrationTimeout)
	t.Cleanup(cancel)
	fixture, err := filepath.Abs(filepath.Join("..", "..", "testdata", "fixtures", fixtureName))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(fixture, "meta.toml")); err != nil {
		t.Fatalf("fixture %s: %v", fixtureName, err)
	}
	home := t.TempDir()
	repo, err := makeGitBasic(ctx, home)
	if err != nil {
		t.Fatal(err)
	}
	if repoForgeToml != "" {
		commitFile(t, ctx, repo, "forge.toml", repoForgeToml)
	}
	if err := writeCaseConfig(home, forgeBin, "git-basic", repo); err != nil {
		t.Fatal(err)
	}
	env := caseEnv(home, fixture)
	stop, err := startDaemon(ctx, forgeBin, home, env)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(stop)
	h := &intHarness{t: t, ctx: ctx, home: home, repo: repo, env: env, http: unixClient(home)}
	h.awaitWorker()
	return h
}

// unixClient speaks HTTP to the home's forge.sock — the transport the CLI, the
// UI, and every plugin use, and the one that needs no token.
func unixClient(home string) *http.Client {
	sock := filepath.Join(home, "forge.sock")
	return &http.Client{
		Transport: &http.Transport{DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			var d net.Dialer
			return d.DialContext(ctx, "unix", sock)
		}},
		Timeout: 30 * time.Second,
	}
}

// commitFile writes one file into the repository and pushes it, so the base
// branch a worktree is cut from already contains it.
func commitFile(t *testing.T, ctx context.Context, repo, name, content string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(repo, name), []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	env := append(os.Environ(),
		"GIT_AUTHOR_NAME=forge-eval", "GIT_AUTHOR_EMAIL=eval@localhost",
		"GIT_COMMITTER_NAME=forge-eval", "GIT_COMMITTER_EMAIL=eval@localhost",
		"GIT_CONFIG_GLOBAL=/dev/null", "GIT_CONFIG_SYSTEM=/dev/null")
	for _, args := range [][]string{
		{"add", "-A"},
		{"-c", "commit.gpgsign=false", "commit", "-q", "-m", "chore: add " + name},
		{"push", "-q", "origin", "main"},
	} {
		cmd := exec.CommandContext(ctx, "git", args...)
		cmd.Dir, cmd.Env = repo, env
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %s: %v: %s", strings.Join(args, " "), err, bytes.TrimSpace(out))
		}
	}
}

// do is one JSON call over the socket. Anything the daemon refuses fails the
// test with its body: an integration test has no "expected" 4xx.
func (h *intHarness) do(method, path string, in, out any) {
	h.t.Helper()
	var body io.Reader
	if in != nil {
		b, err := json.Marshal(in)
		if err != nil {
			h.t.Fatal(err)
		}
		body = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(h.ctx, method, "http://forge"+path, body)
	if err != nil {
		h.t.Fatal(err)
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := h.http.Do(req)
	if err != nil {
		h.t.Fatalf("%s %s: %v\n%s", method, path, err, h.daemonLog())
	}
	raw, err := io.ReadAll(resp.Body)
	if cerr := resp.Body.Close(); cerr != nil {
		h.t.Error(cerr)
	}
	if err != nil {
		h.t.Fatal(err)
	}
	if resp.StatusCode >= 300 {
		h.t.Fatalf("%s %s = %d %s", method, path, resp.StatusCode, bytes.TrimSpace(raw))
	}
	if out != nil && len(raw) > 0 {
		if err := json.Unmarshal(raw, out); err != nil {
			h.t.Fatalf("%s %s: decode %s: %v", method, path, bytes.TrimSpace(raw), err)
		}
	}
}

// daemonLog is the tail of the daemon child's stdio, appended to every failure
// so a broken case is diagnosable without re-running it.
func (h *intHarness) daemonLog() string {
	return tailOf(filepath.Join(h.home, "logs", "eval-daemon.stdio.log"))
}

// awaitWorker blocks until the worker the daemon spawned has registered and
// its repository is in the store — a task submitted before that would sit
// pending with no eligible worker and only time out.
func (h *intHarness) awaitWorker() {
	h.t.Helper()
	deadline := time.Now().Add(integrationTimeout)
	for {
		var workers []struct {
			Connected bool `json:"connected"`
		}
		h.do(http.MethodGet, "/api/v1/workers", nil, &workers)
		var repos []struct {
			Name     string `json:"name"`
			WorkerID string `json:"worker_id"`
		}
		h.do(http.MethodGet, "/api/v1/repositories", nil, &repos)
		for _, w := range workers {
			if !w.Connected {
				continue
			}
			for _, r := range repos {
				if r.Name == "git-basic" && r.WorkerID != "" {
					return
				}
			}
		}
		if time.Now().After(deadline) {
			h.t.Fatalf("no connected worker advertised git-basic within %s\n%s", integrationTimeout, h.daemonLog())
		}
		time.Sleep(100 * time.Millisecond)
	}
}

// submit is POST /api/v1/tasks: the ad-hoc submission a plugin or the UI makes.
func (h *intHarness) submit(prompt, autonomy string) string {
	h.t.Helper()
	var created struct {
		Work struct {
			ID string `json:"id"`
		} `json:"work"`
		Targets []json.RawMessage `json:"targets"`
	}
	h.do(http.MethodPost, "/api/v1/tasks", map[string]any{
		"prompt":       prompt,
		"repositories": []string{"git-basic"},
		"mode":         "run",
		"model":        "haiku",
		"autonomy":     autonomy,
	}, &created)
	if created.Work.ID == "" || len(created.Targets) != 1 {
		h.t.Fatalf("POST /api/v1/tasks created %+v", created)
	}
	return created.Work.ID
}

func (h *intHarness) task(id string) *intTask {
	h.t.Helper()
	var v intTask
	h.do(http.MethodGet, "/api/v1/tasks/"+id, nil, &v)
	return &v
}

// await polls the task until ok reports the state it is waiting for.
func (h *intHarness) await(id, what string, ok func(*intTask) bool) *intTask {
	h.t.Helper()
	deadline := time.Now().Add(integrationTimeout)
	for {
		v := h.task(id)
		if ok(v) {
			return v
		}
		if time.Now().After(deadline) {
			h.t.Fatalf("task %s never reached %s: state %q targets %+v\n%s", id, what, v.State, v.Targets, h.daemonLog())
		}
		select {
		case <-h.ctx.Done():
			h.t.Fatalf("task %s: %v while waiting for %s", id, h.ctx.Err(), what)
		case <-time.After(150 * time.Millisecond):
		}
	}
}

func (h *intHarness) awaitTerminal(id string) *intTask {
	h.t.Helper()
	return h.await(id, "a terminal state", func(v *intTask) bool { return terminalStates[v.State] })
}

// branchLog is `git log --oneline` for a branch of the case repository, so the
// worker's commit can be checked where it actually landed.
func (h *intHarness) branchLog(branch string) string {
	h.t.Helper()
	cmd := exec.CommandContext(h.ctx, "git", "log", "--oneline", branch)
	cmd.Dir = h.repo
	out, err := cmd.CombinedOutput()
	if err != nil {
		h.t.Fatalf("git log %s: %v: %s", branch, err, bytes.TrimSpace(out))
	}
	return string(out)
}

// TestIntegrationSuccessVerifies is (a): a task submitted over the socket runs
// the commit fixture, verifies at L1, and lands succeeded with the commit on
// its own branch and the agent's events in the store.
func TestIntegrationSuccessVerifies(t *testing.T) {
	h := startIntegration(t, "commit", "")
	id := h.submit("Create FORGE_SMOKE.txt with today's date and commit it. Do not push.", "auto")

	v := h.awaitTerminal(id)
	if v.State != "succeeded" {
		t.Fatalf("work state = %q, want succeeded (targets %+v)\n%s", v.State, v.Targets, h.daemonLog())
	}
	if v.Targets[0].State != "succeeded" {
		t.Errorf("target state = %q, want succeeded", v.Targets[0].State)
	}
	a := v.attempt(t)
	if a.VerificationPassed == nil || !*a.VerificationPassed {
		t.Errorf("verification_passed = %v, want true", a.VerificationPassed)
	}
	if a.VerificationLevel == nil || *a.VerificationLevel < 1 {
		t.Errorf("verification_level = %v, want >= 1", a.VerificationLevel)
	}
	if a.Git.Commits != 1 {
		t.Errorf("git commits = %d, want 1", a.Git.Commits)
	}
	if a.NumTurns == 0 || a.Launches != 1 {
		t.Errorf("turns = %d, launches = %d", a.NumTurns, a.Launches)
	}
	var env struct {
		Changes []struct {
			Path string `json:"path"`
		} `json:"changes"`
		Claims []json.RawMessage `json:"claims"`
	}
	if err := json.Unmarshal(a.Result, &env); err != nil {
		t.Fatalf("result envelope: %v: %s", err, a.Result)
	}
	if len(env.Claims) == 0 || len(env.Changes) != 1 || env.Changes[0].Path != "FORGE_SMOKE.txt" {
		t.Errorf("result envelope = %s", a.Result)
	}
	// The commit is on the attempt's own branch, and main is untouched.
	var branch struct {
		Attempt struct {
			Branch string `json:"branch"`
		} `json:"attempt"`
	}
	h.do(http.MethodGet, "/api/v1/attempts/"+a.ID, nil, &branch)
	if branch.Attempt.Branch == "" {
		t.Fatal("attempt has no branch")
	}
	if log := h.branchLog(branch.Attempt.Branch); !strings.Contains(log, "chore: forge smoke test") {
		t.Errorf("branch %s log = %q", branch.Attempt.Branch, log)
	}
	if log := h.branchLog("main"); strings.Contains(log, "chore: forge smoke test") {
		t.Errorf("main moved: %q", log)
	}
	// The worker's event stream reached the store through the daemon: the
	// agent span closed cleanly and the fixture's tool calls are spans on it.
	var events []struct {
		Source string         `json:"source"`
		Kind   string         `json:"kind"`
		SpanID string         `json:"span_id"`
		Attrs  map[string]any `json:"attrs"`
	}
	h.do(http.MethodGet, "/api/v1/attempts/"+a.ID+"/events", nil, &events)
	var agentSpan, toolSpans int
	for _, e := range events {
		if e.Source != "worker" {
			continue
		}
		if e.Kind == "span_end" && e.SpanID == "agent-1" {
			agentSpan++
		}
		if e.Kind == "span_start" && e.Attrs["tool"] != nil {
			toolSpans++
		}
	}
	if agentSpan != 1 || toolSpans == 0 {
		t.Errorf("stored events: %d agent spans, %d tool spans, %d events total", agentSpan, toolSpans, len(events))
	}
	// The fixture committed but never pushed, so the worktree is kept for a
	// human (DESIGN.md retention rules) rather than removed.
	if a.Cleanup.Outcome != "retained" {
		t.Errorf("cleanup = %+v, want retained", a.Cleanup)
	}
	if !v.Targets[0].Retained {
		t.Error("target is not marked retained")
	}
}

// TestIntegrationFailedCheckIsUnverified is (b): the agent reports a clean
// read-only success, but the repository's declared check fails when Forge runs
// it, so the target lands unverified with the check named — the verification
// path the golden cases do not cover.
func TestIntegrationFailedCheckIsUnverified(t *testing.T) {
	h := startIntegration(t, "inventory", "[checks]\nunit = [\"sh\", \"-c\", \"echo boom; exit 1\"]\n")
	id := h.submit("List the files in this repository and summarise what it is.", "auto")

	v := h.awaitTerminal(id)
	if v.State != "unverified" {
		t.Fatalf("work state = %q, want unverified (targets %+v)\n%s", v.State, v.Targets, h.daemonLog())
	}
	if got := v.Targets[0].UnverifiedReason; got != "check_failed:unit" {
		t.Errorf("unverified_reason = %q, want check_failed:unit", got)
	}
	a := v.attempt(t)
	if a.VerificationPassed == nil || *a.VerificationPassed {
		t.Errorf("verification_passed = %v, want false", a.VerificationPassed)
	}
	if a.VerificationLevel == nil || *a.VerificationLevel != 1 {
		t.Errorf("verification_level = %v, want 1 (the check ran)", a.VerificationLevel)
	}
	// The agent itself succeeded: unverified is Forge's verdict, not the
	// agent's exit code.
	if a.NumTurns == 0 {
		t.Errorf("turns = %d, want the agent's own run to be recorded", a.NumTurns)
	}
}

// TestIntegrationQuestionAnsweredResumes is (c): the fixture pauses with a
// question, the target parks in waiting_human, the answer is posted over the
// API, and the worker resumes the same session to a verified success.
func TestIntegrationQuestionAnsweredResumes(t *testing.T) {
	h := startIntegration(t, "question-answered", "")
	id := h.submit("Improve the README.", "checkpoint")

	v := h.await(id, "waiting_human", func(v *intTask) bool {
		return len(v.Targets) == 1 && v.Targets[0].State == "waiting_human" && len(v.Questions) == 1
	})
	q := v.Questions[0]
	if q.Text != "Which one?" || len(q.Options) != 2 {
		t.Errorf("question = %+v", q)
	}
	if !q.AnsweredAt.IsZero() {
		t.Errorf("question is already answered: %+v", q)
	}
	paused := v.attempt(t)
	if paused.SessionID == "" {
		t.Fatal("the paused attempt recorded no session id to resume")
	}

	h.do(http.MethodPost, "/api/v1/questions/"+q.ID+"/answer", map[string]any{
		"answer": "docs/README.md",
		"by":     "integration-test",
	}, nil)

	v = h.awaitTerminal(id)
	if v.State != "succeeded" {
		t.Fatalf("work state after the answer = %q, want succeeded (targets %+v)\n%s", v.State, v.Targets, h.daemonLog())
	}
	a := v.attempt(t)
	if a.Launches < 2 {
		t.Errorf("launches = %d, want >= 2 (the resume is a second launch)", a.Launches)
	}
	if a.SessionID != paused.SessionID {
		t.Errorf("resumed session = %q, want the paused attempt's %q", a.SessionID, paused.SessionID)
	}
	if a.VerificationPassed == nil || !*a.VerificationPassed {
		t.Errorf("verification_passed = %v, want true", a.VerificationPassed)
	}
	if len(v.Questions) != 1 {
		t.Fatalf("questions = %d, want 1", len(v.Questions))
	}
	answered := v.Questions[0]
	if answered.Answer != "docs/README.md" || answered.AnsweredBy != "integration-test" || answered.AnsweredAt.IsZero() {
		t.Errorf("answered question = %+v", answered)
	}
	var env struct {
		NeedsInput json.RawMessage `json:"needs_input"`
	}
	if err := json.Unmarshal(a.Result, &env); err != nil {
		t.Fatalf("result envelope: %v: %s", err, a.Result)
	}
	if s := strings.TrimSpace(string(env.NeedsInput)); s != "null" && s != "" {
		t.Errorf("the resumed result still asks a question: %s", a.Result)
	}
}
