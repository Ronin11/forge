package worker

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	"forge/internal/logging"
	"forge/internal/model"
	"forge/internal/protocol"
)

// reconcileDaemon extends fakeDaemon with the reconcile calls.
type reconcileDaemon struct {
	fakeDaemon
	mu               sync.Mutex
	refuseEmptyLease bool
	states           map[string]*AttemptState
	patched          map[string]protocol.CleanupPatch
	complete         map[string]protocol.CompleteRequest
}

func (d *reconcileDaemon) Register(context.Context, protocol.RegisterRequest) (*protocol.RegisterResponse, error) {
	return &protocol.RegisterResponse{}, nil
}
func (d *reconcileDaemon) Claim(context.Context, protocol.ClaimRequest) (*protocol.Claim, error) {
	return nil, nil
}
func (d *reconcileDaemon) Attempt(_ context.Context, id string) (*AttemptState, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if st, ok := d.states[id]; ok {
		return st, nil
	}
	return nil, &StatusError{Status: 404, Message: "unknown attempt"}
}
func (d *reconcileDaemon) PatchCleanup(_ context.Context, id string, p protocol.CleanupPatch) error {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.patched[id] = p
	return nil
}
func (d *reconcileDaemon) Complete(_ context.Context, id string, req protocol.CompleteRequest) (*protocol.CompleteResponse, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.refuseEmptyLease && req.LeaseToken == "" {
		return nil, &StatusError{Status: 400, Message: "lease_token is required"}
	}
	d.complete[id] = req
	return &protocol.CompleteResponse{State: req.State}, nil
}

func newReconcileWorker(t *testing.T) (*Worker, *gitFixture, *reconcileDaemon) {
	t.Helper()
	gf := newGitFixture(t)
	home := t.TempDir()
	cfg := &Config{Daemon: "unix://" + filepath.Join(home, "forge.sock"), DataDir: filepath.Join(home, "worker"), MaxConcurrent: 1, Name: "test",
		Executors:    map[string]ExecutorConfig{"fake-claude": {Command: []string{forgeBin, "fake-claude"}, Output: "claude-stream-json"}},
		Repositories: map[string]RepositoryConfig{"demo": {Path: gf.checkout, BaseBranch: "master", Project: "default"}}}
	cfg.path = filepath.Join(home, "worker.toml")
	d := &reconcileDaemon{states: map[string]*AttemptState{}, patched: map[string]protocol.CleanupPatch{}, complete: map[string]protocol.CompleteRequest{}}
	w, err := New(context.Background(), WorkerOptions{Config: cfg, Version: "test", Handler: logging.Discard(), ForgeBin: forgeBin, Daemon: d})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := w.Close(); err != nil {
			t.Error(err)
		}
	})
	return w, gf, d
}

// orphanedAttempt fakes a worker that died mid-agent: a manifest in running
// with a real live process group, a worktree with an unpushed commit.
func orphanedAttempt(t *testing.T, w *Worker, gf *gitFixture, dirty bool) (*Manifest, *exec.Cmd) {
	t.Helper()
	ctx := context.Background()
	repo := w.runner.repos["demo"]
	_, commit, err := w.runner.git.ResolveBase(ctx, repo, "")
	if err != nil {
		t.Fatal(err)
	}
	id := model.NewID()
	wt := filepath.Join(w.cfg.DataDir, "worktrees", id)
	if err := os.MkdirAll(filepath.Dir(wt), 0o700); err != nil {
		t.Fatal(err)
	}
	branch := model.BranchName("demo", id)
	if err := w.runner.git.WorktreeAdd(ctx, repo, wt, branch, commit); err != nil {
		t.Fatal(err)
	}
	if dirty {
		gf.write(t, filepath.Join(wt, "scratch.txt"), "unsaved\n")
	}
	child := exec.Command("sh", "-c", "sleep 300")
	child.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := child.Start(); err != nil {
		t.Fatal(err)
	}
	start, err := ProcessStart(child.Process.Pid)
	if err != nil {
		t.Fatal(err)
	}
	m := &Manifest{AttemptID: id, TargetID: model.NewID(), WorkID: model.NewID(), RoutineName: "demo", RepositoryName: "demo", RepositoryPath: repo.Path, OriginIdentity: repo.OriginIdentity,
		BaseBranch: "master", BaseCommit: commit, WorktreePath: wt, Branch: branch, Kind: "attempt", PID: child.Process.Pid, PIDStart: start, ProcessActive: true, Lifecycle: ManifestRunning, Launches: 1}
	if err := w.runner.manifests.Write(m); err != nil {
		t.Fatal(err)
	}
	return m, child
}

func TestReconcileKillsOrphanAndReports(t *testing.T) {
	w, gf, d := newReconcileWorker(t)
	clean, child1 := orphanedAttempt(t, w, gf, false)
	dirty, child2 := orphanedAttempt(t, w, gf, true)
	d.states[clean.AttemptID] = &AttemptState{AttemptID: clean.AttemptID, TargetState: "running"}
	d.states[dirty.AttemptID] = &AttemptState{AttemptID: dirty.AttemptID, TargetState: "failed", Terminal: true} // the sweeper already closed it
	if err := w.reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	for _, c := range []*exec.Cmd{child1, child2} {
		waitErr := make(chan error, 1)
		go func() { waitErr <- c.Wait() }()
		select {
		case <-waitErr:
		case <-time.After(10 * time.Second):
			t.Fatalf("orphan %d survived reconcile", c.Process.Pid)
		}
	}
	m1, err := w.runner.manifests.Load(clean.AttemptID)
	if err != nil || m1.Lifecycle != ManifestCleaned || m1.ProcessActive {
		t.Errorf("clean orphan manifest = %+v %v", m1, err)
	}
	if req, ok := d.complete[clean.AttemptID]; !ok || req.State != model.Failed || req.FailureReason != model.ReasonWorkerRestart || req.Cleanup.Outcome != "removed" {
		t.Errorf("clean orphan completion = %+v", req)
	}
	m2, err := w.runner.manifests.Load(dirty.AttemptID)
	if err != nil || m2.Lifecycle != ManifestRetained || m2.RetentionReason != "dirty worktree" {
		t.Errorf("dirty orphan manifest = %+v %v", m2, err)
	}
	if p, ok := d.patched[dirty.AttemptID]; !ok || p.Cleanup.Outcome != "retained" || !p.Git.Dirty {
		t.Errorf("dirty orphan patch = %+v", p)
	}
	if _, ok := d.complete[dirty.AttemptID]; ok {
		t.Error("a Target the daemon closed must not be completed again")
	}
	retained := w.retained.list()
	if len(retained) != 1 || retained[0].AttemptID != dirty.AttemptID || !strings.HasPrefix(retained[0].CleanupCommand, "forge cleanup ") {
		t.Errorf("retained = %+v", retained)
	}
	reg := w.registerRequest()
	if len(reg.Retained) != 1 || len(reg.Repositories) != 1 || reg.Capabilities["sandbox"] == "" {
		t.Errorf("registration = %+v", reg)
	}
	// Idle: a second pass touches nothing.
	if err := w.reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
}

// TestReconcileFallsBackToPatchOn400 pins the smoke-7 finding: a worker_restart
// completion has no lease token (manifests never store one); when the daemon
// refuses it with 400, the cleanup fields must still land via the patch.
func TestReconcileFallsBackToPatchOn400(t *testing.T) {
	w, gf, d := newReconcileWorker(t)
	d.refuseEmptyLease = true
	m, child := orphanedAttempt(t, w, gf, true)
	if err := child.Process.Kill(); err != nil {
		t.Fatal(err)
	}
	if err := child.Wait(); err == nil {
		t.Fatal("expected a signal exit")
	}
	m.ProcessActive, m.PID, m.PIDStart = false, 0, 0
	if err := w.runner.manifests.Write(m); err != nil {
		t.Fatal(err)
	}
	d.states[m.AttemptID] = &AttemptState{AttemptID: m.AttemptID, TargetState: "running"}
	if err := w.reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if _, ok := d.complete[m.AttemptID]; ok {
		t.Error("complete must not land without a lease")
	}
	if p, ok := d.patched[m.AttemptID]; !ok || p.Cleanup.Outcome != "retained" {
		t.Errorf("cleanup patch = %+v", p)
	}
}

func TestReconcileLeavesResumableAlone(t *testing.T) {
	w, gf, d := newReconcileWorker(t)
	m, child := orphanedAttempt(t, w, gf, false)
	if err := child.Process.Kill(); err != nil {
		t.Fatal(err)
	}
	if err := child.Wait(); err == nil {
		t.Fatal("expected a signal exit")
	}
	m.ProcessActive, m.PID, m.PIDStart, m.Lifecycle, m.Resumable = false, 0, 0, ManifestExited, true
	if err := w.runner.manifests.Write(m); err != nil {
		t.Fatal(err)
	}
	d.states[m.AttemptID] = &AttemptState{AttemptID: m.AttemptID, TargetState: "waiting_human", Resumable: true}
	if err := w.reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	got, err := w.runner.manifests.Load(m.AttemptID)
	if err != nil || got.Lifecycle != ManifestExited {
		t.Errorf("resumable manifest touched: %+v %v", got, err)
	}
	if _, err := os.Stat(m.WorktreePath); err != nil {
		t.Error("resumable worktree removed")
	}
}
