package worker

import (
	"context"
	"encoding/json"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"forge/internal/logging"
	"forge/internal/model"
	"forge/internal/protocol"
)

// forgeBin is the freshly built binary every attempt test launches as the
// fake-claude executor: a real child process through the real launch path.
var forgeBin string

func TestMain(m *testing.M) {
	dir, err := os.MkdirTemp("", "forge-worker-test")
	if err != nil {
		panic(err)
	}
	forgeBin = filepath.Join(dir, "forge")
	build := exec.Command("go", "build", "-o", forgeBin, "forge/cmd/forge")
	build.Stderr = os.Stderr
	if err := build.Run(); err != nil {
		panic("build forge for tests: " + err.Error())
	}
	code := m.Run()
	if err := os.RemoveAll(dir); err != nil {
		panic(err)
	}
	os.Exit(code)
}

// fakeDaemon records what the worker sends and answers like the daemon would.
type fakeDaemon struct {
	mu         sync.Mutex
	events     []protocol.Event
	heartbeats []protocol.HeartbeatRequest
	complete   *protocol.CompleteRequest
	cancelAt   int // cancel on the nth heartbeat (0 = never)
	failEvents bool
}

func (d *fakeDaemon) Events(_ context.Context, _ string, b protocol.EventBatch) error {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.failEvents {
		return &StatusError{Status: 500, Message: "down"}
	}
	d.events = append(d.events, b.Events...)
	return nil
}

func (d *fakeDaemon) Heartbeat(_ context.Context, _ string, req protocol.HeartbeatRequest) (*protocol.HeartbeatResponse, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.heartbeats = append(d.heartbeats, req)
	return &protocol.HeartbeatResponse{CancelRequested: d.cancelAt > 0 && len(d.heartbeats) >= d.cancelAt, LeaseExpiresAt: time.Now().Add(30 * time.Second)}, nil
}

func (d *fakeDaemon) Complete(_ context.Context, _ string, req protocol.CompleteRequest) (*protocol.CompleteResponse, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.complete = &req
	return &protocol.CompleteResponse{State: req.State}, nil
}

func (d *fakeDaemon) spans() map[string]int64 {
	d.mu.Lock()
	defer d.mu.Unlock()
	out := map[string]int64{}
	for _, e := range d.events {
		if e.Kind == protocol.KindSpanEnd {
			out[e.Name] = e.DurationUS
		}
	}
	return out
}

// runnerFixture is a Runner over a real temporary origin+checkout and the
// fake-claude executor pinned to one fixture.
type runnerFixture struct {
	t       *testing.T
	git     *gitFixture
	dataDir string
	daemon  *fakeDaemon
	runner  *Runner
}

func newRunnerFixture(t *testing.T, fixture string) *runnerFixture {
	t.Helper()
	gf := newGitFixture(t)
	dataDir := filepath.Join(t.TempDir(), "worker")
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		t.Fatal(err)
	}
	fixtures, err := filepath.Abs("../../testdata/fixtures")
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("FORGE_FAKE_FIXTURE", filepath.Join(fixtures, fixture))
	cfg := &Config{Daemon: "unix://" + filepath.Join(dataDir, "..", "forge.sock"), DataDir: dataDir, MaxConcurrent: 1,
		Executors: map[string]ExecutorConfig{"fake-claude": {Command: []string{forgeBin, "fake-claude", "--fixture", "{{fixture}}", "--model", "{{model}}", "--mcp-config", "{{mcp_config}}"}, Output: "claude-stream-json", Capabilities: []string{"resume"}}}}
	execs, err := ExecutorsFromConfig(cfg.Executors)
	if err != nil {
		t.Fatal(err)
	}
	manifests, err := NewManifestStore(dataDir, testWorkerID, time.Now)
	if err != nil {
		t.Fatal(err)
	}
	d := &fakeDaemon{}
	h := logging.New(os.Stderr, logging.Options{Levels: logging.Levels{Default: slog.LevelWarn}, Format: logging.FormatText}, nil)
	r := &Runner{cfg: cfg, workerID: testWorkerID, git: gf.g, executors: execs, parsers: DefaultParsers(), manifests: manifests, daemon: d, repos: map[string]*Repository{"demo": gf.repo}, log: h.For("worker.attempt"), clock: time.Now, forgeBin: forgeBin}
	return &runnerFixture{t: t, git: gf, dataDir: dataDir, daemon: d, runner: r}
}

func (f *runnerFixture) claim(autonomy model.Autonomy) *protocol.Claim {
	return &protocol.Claim{AttemptID: model.NewID(), LeaseToken: "lease", TargetID: model.NewID(), WorkID: model.NewID(), RoutineName: "demo", Generation: 1,
		Repository: "demo", Mode: "run", Prompt: "do the thing", Executor: "fake-claude", Model: "claude-haiku-4-5-20251001", MaxTurns: 5, TimeoutSeconds: 60, Autonomy: autonomy, MCPToken: "mcp"}
}

func (f *runnerFixture) run(c *protocol.Claim) protocol.CompleteRequest {
	f.t.Helper()
	f.runner.Run(context.Background(), c)
	f.daemon.mu.Lock()
	defer f.daemon.mu.Unlock()
	if f.daemon.complete == nil {
		f.t.Fatal("attempt never completed")
	}
	return *f.daemon.complete
}

func TestAttemptInventorySucceedsAndCleansUp(t *testing.T) {
	f := newRunnerFixture(t, "inventory")
	c := f.claim(model.AutonomyAuto)
	before := f.git.checkoutSnapshot(t)
	req := f.run(c)
	if req.State != model.Succeeded || req.ExitCode != 0 || req.NumTurns != 2 || req.Usage.OutputTokens != 460 || req.CostUSD == nil {
		t.Errorf("complete = state %s exit %d turns %d usage %+v", req.State, req.ExitCode, req.NumTurns, req.Usage)
	}
	if req.Cleanup.Outcome != "removed" || req.Git.Dirty || req.Git.Commits != 0 || !req.Git.Pushed {
		t.Errorf("cleanup %+v git %+v", req.Cleanup, req.Git)
	}
	if !req.Verification.Passed || req.Verification.Level != 1 {
		t.Errorf("verification %+v", req.Verification)
	}
	if req.Launches != 1 || req.SessionID == "" || len(req.Result) == 0 {
		t.Errorf("launches %d session %q result %d bytes", req.Launches, req.SessionID, len(req.Result))
	}
	if after := f.git.checkoutSnapshot(t); after != before {
		t.Errorf("checkout changed:\n%s\n---\n%s", before, after)
	}
	spans := f.daemon.spans()
	for _, name := range []string{"fetch", "resolve_base", "worktree_add", "manifest", "agent-1", "git_inspect", "verify", "cleanup"} {
		if spans[name] <= 0 {
			t.Errorf("phase span %s missing or zero: %v", name, spans)
		}
	}
	if spans["Bash"] <= 0 {
		t.Errorf("tool span Bash missing: %v", spans)
	}
	m, err := f.runner.manifests.Load(c.AttemptID)
	if err != nil || m.Lifecycle != ManifestCleaned || m.ProcessActive {
		t.Errorf("manifest = %+v, %v", m, err)
	}
	if _, err := os.Stat(filepath.Join(f.dataDir, "worktrees", c.AttemptID)); !os.IsNotExist(err) {
		t.Error("worktree not removed")
	}
	if _, err := os.Stat(filepath.Join(f.dataDir, "output", c.AttemptID+".log")); err != nil {
		t.Error("output file missing")
	}
	var hb []string
	for _, h := range f.daemon.heartbeats {
		if h.State != "" {
			hb = append(hb, string(h.State))
		}
	}
	if got := strings.Join(hb, ","); got != "preparing,preparing,running" {
		t.Errorf("heartbeat states = %s", got)
	}
}

func TestAttemptCommitIsRetainedWithUnpushedCommits(t *testing.T) {
	f := newRunnerFixture(t, "commit")
	c := f.claim(model.AutonomyAuto)
	req := f.run(c)
	if req.State != model.Succeeded {
		t.Fatalf("state = %s (%s)", req.State, req.FailureReason)
	}
	if req.Git.Commits != 1 || req.Git.Pushed || req.Git.Dirty || len(req.Git.ChangedPaths) != 1 || req.Git.ChangedPaths[0] != "FORGE_SMOKE.txt" {
		t.Errorf("git = %+v", req.Git)
	}
	if req.Cleanup.Outcome != "retained" || req.Cleanup.Reason != "unpushed commits" || !strings.HasPrefix(req.Cleanup.Command, "forge cleanup ") {
		t.Errorf("cleanup = %+v", req.Cleanup)
	}
	if !req.Verification.Passed {
		t.Errorf("L0 should pass: %s %s", req.Verification.Reason, req.Verification.Verdict)
	}
	branches, err := f.git.g.Run(context.Background(), f.git.checkout, "branch", "--list", "forge/*")
	if err != nil || !strings.Contains(branches, "forge/demo-"+c.AttemptID[:8]) {
		t.Errorf("branch missing: %q %v", branches, err)
	}
	if _, err := os.Stat(filepath.Join(f.dataDir, "worktrees", c.AttemptID, "FORGE_SMOKE.txt")); err != nil {
		t.Error("retained worktree lost the file")
	}
}

func TestAttemptFailingExit(t *testing.T) {
	f := newRunnerFixture(t, "failing")
	req := f.run(f.claim(model.AutonomyAuto))
	if req.State != model.Failed || req.FailureReason != model.ReasonExitNonzero || req.ExitCode != 1 || !req.IsError {
		t.Errorf("complete = %s/%s exit %d is_error %v", req.State, req.FailureReason, req.ExitCode, req.IsError)
	}
	if req.Cleanup.Outcome != "removed" {
		t.Errorf("a clean failed worktree is removed: %+v", req.Cleanup)
	}
}

func TestAttemptNeedsInputThenResume(t *testing.T) {
	f := newRunnerFixture(t, "needs-input")
	c := f.claim(model.AutonomyAsk)
	req := f.run(c)
	if req.State != model.WaitingHuman || req.Question == nil || req.Question.Text != "Which one?" {
		t.Fatalf("complete = %s question %+v", req.State, req.Question)
	}
	if req.Cleanup.Outcome != "kept" {
		t.Errorf("worktree must be kept for resume: %+v", req.Cleanup)
	}
	m, err := f.runner.manifests.Load(c.AttemptID)
	if err != nil || !m.Resumable || m.SessionID == "" || m.NextSeq == 0 {
		t.Fatalf("manifest = %+v %v", m, err)
	}
	firstSeq := m.NextSeq
	f.daemon.complete = nil
	c.Resume = &protocol.Resume{SessionID: m.SessionID, Answer: "docs/README.md", Launches: 1}
	req = f.run(c)
	if req.State != model.Succeeded || req.Launches != 2 || req.SessionID != m.SessionID {
		t.Errorf("resume = %s launches %d session %q", req.State, req.Launches, req.SessionID)
	}
	f.daemon.mu.Lock()
	minSeq := -1
	for _, e := range f.daemon.events {
		if e.Kind == protocol.KindLifecycle && strings.HasPrefix(e.Message, "resuming") {
			minSeq = e.Seq
		}
	}
	f.daemon.mu.Unlock()
	if minSeq < firstSeq {
		t.Errorf("resume did not continue seq: %d < %d", minSeq, firstSeq)
	}
	auto := newRunnerFixture(t, "needs-input")
	if req := auto.run(auto.claim(model.AutonomyAuto)); req.State != model.Failed || req.FailureReason != model.ReasonAmbiguityAtAuto {
		t.Errorf("needs_input at auto = %s/%s", req.State, req.FailureReason)
	}
}

func TestAttemptTimeoutKillsAgent(t *testing.T) {
	f := newRunnerFixture(t, "timeout")
	c := f.claim(model.AutonomyAuto)
	c.TimeoutSeconds = 1
	start := time.Now()
	req := f.run(c)
	if req.State != model.Failed || req.FailureReason != model.ReasonTimeout {
		t.Errorf("complete = %s/%s", req.State, req.FailureReason)
	}
	if time.Since(start) > 20*time.Second {
		t.Errorf("timeout took %s", time.Since(start))
	}
}

func TestAttemptCancelViaHeartbeat(t *testing.T) {
	f := newRunnerFixture(t, "timeout")
	f.daemon.cancelAt = 1
	c := f.claim(model.AutonomyAuto)
	c.TimeoutSeconds = 120
	req := f.run(c)
	if req.State != model.Cancelled || req.FailureReason != model.ReasonCancelled {
		t.Errorf("complete = %s/%s", req.State, req.FailureReason)
	}
}

func TestAttemptToolErrorAndRateLimit(t *testing.T) {
	f := newRunnerFixture(t, "tool-error")
	req := f.run(f.claim(model.AutonomyAuto))
	if req.State != model.Succeeded {
		t.Fatalf("state = %s", req.State)
	}
	f.daemon.mu.Lock()
	defer f.daemon.mu.Unlock()
	var sawRate, sawErr bool
	for _, e := range f.daemon.events {
		if e.Kind == protocol.KindMetric && e.Name == "rate_limit" {
			sawRate = true
		}
		if e.Kind == protocol.KindSpanEnd && e.Name == "Read" {
			var attrs struct {
				IsError bool `json:"is_error"`
			}
			if json.Unmarshal(e.Attrs, &attrs) == nil && attrs.IsError {
				sawErr = true
			}
		}
	}
	if !sawRate || !sawErr {
		t.Errorf("rate=%v toolErr=%v", sawRate, sawErr)
	}
}

func TestAttemptDeclaredChecksL1(t *testing.T) {
	f := newRunnerFixture(t, "inventory")
	f.git.write(t, filepath.Join(f.git.checkout, "forge.toml"), "[checks]\nok = [\"true\"]\nbad = [\"sh\", \"-c\", \"echo '--- FAIL: TestX'; exit 1\"]\n")
	f.git.run(t, f.git.checkout, "add", "forge.toml")
	f.git.run(t, f.git.checkout, "commit", "-q", "-m", "checks")
	f.git.run(t, f.git.checkout, "push", "-q", "origin", "master")
	req := f.run(f.claim(model.AutonomyAuto))
	if req.State != model.Succeeded || req.Verification.Passed || req.Verification.Reason != "check_failed:bad" {
		t.Errorf("verification = %+v (state %s)", req.Verification, req.State)
	}
	if !strings.Contains(string(req.Verification.Verdict), "TestX") {
		t.Errorf("failing test not extracted: %s", req.Verification.Verdict)
	}
}
