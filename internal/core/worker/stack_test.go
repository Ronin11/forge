package worker

import (
	"context"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
)

// A stacked claim (DESIGN.md §20) cuts its worktree at the dependency's
// branch head instead of origin/<base>; the manifest records that base, and a
// missing stack commit fails prepare rather than guessing.
func TestPrepareWorktreeStackBase(t *testing.T) {
	ctx := context.Background()
	gf := newGitFixture(t)
	// The "dependency's task branch": one commit beyond master, in a linked
	// worktree the way a worker attempt makes it.
	wt := filepath.Join(gf.root, "dep-wt")
	gf.run(t, gf.checkout, "worktree", "add", "-b", "forge/dep-1", wt, "master")
	gf.write(t, filepath.Join(wt, "dep.txt"), "dep work\n")
	gf.run(t, wt, "add", "dep.txt")
	gf.run(t, wt, "commit", "-qm", "dep")
	depHead := gf.run(t, wt, "rev-parse", "HEAD")
	gf.run(t, gf.checkout, "worktree", "remove", "--force", wt)

	dataDir := filepath.Join(t.TempDir(), "worker")
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		t.Fatal(err)
	}
	manifests, err := NewManifestStore(dataDir, testWorkerID, time.Now)
	if err != nil {
		t.Fatal(err)
	}
	r := &Runner{cfg: &Config{Daemon: "unix://x", DataDir: dataDir, MaxConcurrent: 1}, workerID: testWorkerID,
		git: gf.g, manifests: manifests, daemon: &fakeDaemon{}, repos: map[string]*Repository{"demo": gf.repo},
		log: slog.New(slog.DiscardHandler), clock: time.Now}

	c := &protocol.Claim{AttemptID: model.NewID(), TargetID: model.NewID(), WorkID: model.NewID(),
		RoutineName: "stacked", Repository: "demo", Mode: "run", TimeoutSeconds: 60, LeaseToken: "lease", MCPToken: "mcp",
		StackBase: &protocol.StackBase{WorkID: "dep-work", Branch: "forge/dep-1", Commit: depHead, Depth: 1}}
	a := bareAttempt(r, c)
	path := filepath.Join(dataDir, "worktrees", c.AttemptID)
	if err := a.prepareWorktree(ctx, path, model.BranchName(c.RoutineName, c.AttemptID)); err != nil {
		t.Fatalf("prepareWorktree: %v", err)
	}
	m := a.manifest
	if m.BaseCommit != depHead || m.BaseBranch != "forge/dep-1" {
		t.Fatalf("manifest base = %s@%s, want forge/dep-1@%s", m.BaseBranch, m.BaseCommit, depHead)
	}
	head := gf.run(t, path, "rev-parse", "HEAD")
	if head != depHead {
		t.Fatalf("worktree HEAD = %s, want the stack base %s", head, depHead)
	}

	// A stack base that does not exist fails prepare loudly.
	c2 := &protocol.Claim{AttemptID: model.NewID(), TargetID: model.NewID(), WorkID: model.NewID(),
		RoutineName: "stacked", Repository: "demo", Mode: "run", TimeoutSeconds: 60, LeaseToken: "lease", MCPToken: "mcp",
		StackBase: &protocol.StackBase{WorkID: "dep-work", Branch: "forge/dep-1", Commit: strings.Repeat("ab", 20), Depth: 1}}
	a2 := bareAttempt(r, c2)
	err = a2.prepareWorktree(ctx, filepath.Join(dataDir, "worktrees", c2.AttemptID), model.BranchName(c2.RoutineName, c2.AttemptID))
	if err == nil || !strings.Contains(err.Error(), "not present") {
		t.Fatalf("missing stack base: err = %v", err)
	}
}
