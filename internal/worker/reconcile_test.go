package worker

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"forge/internal/protocol"
)

type fakeCompleter struct {
	calls map[string]protocol.CompleteRequest
}

func (f *fakeCompleter) Complete(_ context.Context, id string, req protocol.CompleteRequest) error {
	f.calls[id] = req
	return nil
}

// TestReconcileAfterCrash simulates a worker that died mid-attempt: a manifest
// in lifecycle running with a live orphaned process and a worktree in each of
// the cleanup-matrix states.
func TestReconcileAfterCrash(t *testing.T) {
	ctx := context.Background()
	dataDir := t.TempDir()
	store, err := NewManifestStore(dataDir)
	if err != nil {
		t.Fatal(err)
	}
	checkout, _ := newTestRepo(t)
	repo, err := ValidateRepository(ctx, "r", checkout, "")
	if err != nil {
		t.Fatal(err)
	}
	base, err := repo.FetchBase(ctx)
	if err != nil {
		t.Fatal(err)
	}

	// An orphaned process group that must be killed.
	orphan := exec.Command("sh", "-c", "sleep 300 & sleep 300")
	orphan.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := orphan.Start(); err != nil {
		t.Fatal(err)
	}
	go func() { _ = orphan.Wait() }()
	identity, err := processIdentity(orphan.Process.Pid)
	if err != nil {
		t.Fatal(err)
	}

	mk := func(id string, lifecycle string, dirty bool, withWorktree bool, pid int, ident string) Manifest {
		attemptID := strings.Repeat(id, 32)
		wt := filepath.Join(dataDir, "worktrees", "r", attemptID)
		if withWorktree {
			if err := repo.AddWorktree(ctx, wt, "forge/x-"+id, base); err != nil {
				t.Fatal(err)
			}
			if dirty {
				if err := os.WriteFile(filepath.Join(wt, "README"), []byte("changed"), 0o644); err != nil {
					t.Fatal(err)
				}
			}
		}
		m := Manifest{AttemptID: attemptID, TargetID: strings.Repeat("1", 32), WorkID: strings.Repeat("2", 32),
			WorkerID: "w", Repository: "r", RepositoryPath: repo.Path, RemoteIdentity: repo.RemoteIdentity,
			BaseBranch: "main", BaseCommit: base, WorktreePath: wt, Branch: "forge/x-" + id, Lifecycle: lifecycle,
			PID: pid, ProcessIdentity: ident}
		if err := store.Create(m); err != nil {
			t.Fatal(err)
		}
		return m
	}
	running := mk("a", LifecycleRunning, false, true, orphan.Process.Pid, identity)
	dirty := mk("b", LifecycleRunning, true, true, 0, "")
	neverCreated := mk("c", LifecyclePreparing, false, false, 0, "")
	active := mk("d", LifecycleRunning, false, true, 0, "")
	alreadyRetained := mk("e", LifecycleRetained, true, true, 0, "")

	fake := &fakeCompleter{calls: map[string]protocol.CompleteRequest{}}
	var retained []string
	r := &reconciler{workerID: "w", store: store, repos: map[string]Repository{"r": repo}, report: fake,
		isActive: func(id string) bool { return id == active.AttemptID },
		onRetain: func(m Manifest) { retained = append(retained, m.AttemptID) }}
	handled, err := r.run(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if handled != 3 {
		t.Errorf("handled %d want 3", handled)
	}
	for deadline := time.Now().Add(2 * time.Second); processGroupAlive(orphan.Process.Pid) && time.Now().Before(deadline); {
		time.Sleep(20 * time.Millisecond)
	}
	if processGroupAlive(orphan.Process.Pid) {
		t.Error("orphaned process group survived reconcile")
	}

	check := func(m Manifest, lifecycle, outcome string) {
		t.Helper()
		got, err := store.Load(m.AttemptID)
		if err != nil {
			t.Fatal(err)
		}
		if got.Lifecycle != lifecycle || got.PID != 0 {
			t.Errorf("%s: lifecycle %s pid %d, want %s", m.AttemptID[:1], got.Lifecycle, got.PID, lifecycle)
		}
		call, ok := fake.calls[m.AttemptID]
		if !ok || call.State != "failed" || call.Reason != "worker_restart" || call.Cleanup.Outcome != outcome {
			t.Errorf("%s: report %+v", m.AttemptID[:1], call)
		}
	}
	check(running, LifecycleCleaned, "removed")
	check(dirty, LifecycleRetained, "retained")
	check(neverCreated, LifecycleNotCreated, "missing")
	if _, err := os.Stat(running.WorktreePath); !os.IsNotExist(err) {
		t.Error("clean worktree not removed")
	}
	if _, err := os.Stat(dirty.WorktreePath); err != nil {
		t.Error("dirty worktree removed")
	}
	if _, ok := fake.calls[active.AttemptID]; ok {
		t.Error("active attempt reconciled")
	}
	if _, ok := fake.calls[alreadyRetained.AttemptID]; ok {
		t.Error("retained manifest re-reported")
	}
	if len(retained) != 2 {
		t.Errorf("retained advertisements %v", retained)
	}
	if got := mustGit(t, checkout, "status", "--porcelain"); got != "" {
		t.Errorf("checkout dirty: %q", got)
	}
	// A second run is a no-op.
	if handled, err := r.run(ctx); err != nil || handled != 0 {
		t.Errorf("second run handled=%d err=%v", handled, err)
	}
}

func TestLoadConfig(t *testing.T) {
	p := filepath.Join(t.TempDir(), "worker.toml")
	body := `
server = "http://127.0.0.1:7340"
max_concurrent = 2
[executors.claude-code]
command = ["claude", "--model", "{{model}}"]
output = "claude-stream-json"
[repositories.a]
path = "/tmp/a"
`
	if err := os.WriteFile(p, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	c, err := LoadConfig(p)
	if err != nil {
		t.Fatal(err)
	}
	if c.MaxConcurrent != 2 || c.ExecutorNames()[0] != "claude-code" || !strings.HasSuffix(c.DataDir, ".forge/worker") {
		t.Errorf("config %+v", c)
	}
	if err := os.WriteFile(p, []byte(body+"\nbogus = 1\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadConfig(p); err == nil {
		t.Error("unknown field accepted")
	}
}
