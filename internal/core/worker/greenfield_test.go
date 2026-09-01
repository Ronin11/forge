package worker

import (
	"context"
	"encoding/json"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
)

// bareAttempt wires an attempt around a Runner without launching an agent, so
// prepare/cleanup/move can be driven directly.
func bareAttempt(r *Runner, c *protocol.Claim) *attempt {
	a := &attempt{r: r, claim: c, ctx: context.Background(), log: slog.New(slog.DiscardHandler)}
	a.emitter = NewEmitter(c.AttemptID, r.daemon, a.log, r.clock, 0, 0)
	a.repo = r.repos[c.Repository]
	return a
}

// greenfieldRunner is a Runner configured with a projects_root and the virtual
// greenfield repository, like New() builds it.
func greenfieldRunner(t *testing.T) (*Runner, string) {
	t.Helper()
	root := t.TempDir()
	dataDir := filepath.Join(root, "worker")
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		t.Fatal(err)
	}
	projects := filepath.Join(root, "projects")
	cfg := &Config{Daemon: "unix://x", DataDir: dataDir, MaxConcurrent: 1, Greenfield: GreenfieldConfig{ProjectsRoot: projects}}
	manifests, err := NewManifestStore(dataDir, testWorkerID, time.Now)
	if err != nil {
		t.Fatal(err)
	}
	r := &Runner{
		cfg: cfg, workerID: testWorkerID, git: Git{}, manifests: manifests, daemon: &fakeDaemon{},
		repos: map[string]*Repository{greenfieldRepoName: {Name: greenfieldRepoName, Path: projects, OriginIdentity: greenfieldOriginIdentity}},
		log:   slog.New(slog.DiscardHandler), clock: time.Now,
	}
	return r, projects
}

func greenfieldClaim() *protocol.Claim {
	return &protocol.Claim{
		AttemptID: model.NewID(), TargetID: model.NewID(), WorkID: model.NewID(), RoutineName: "greenfield",
		Repository: greenfieldRepoName, Mode: "greenfield", TimeoutSeconds: 60, LeaseToken: "lease", MCPToken: "mcp",
	}
}

func TestGreenfieldPrepareMoveAndCleanup(t *testing.T) {
	ctx := context.Background()
	r, projects := greenfieldRunner(t)
	c := greenfieldClaim()
	a := bareAttempt(r, c)

	wt := filepath.Join(r.cfg.DataDir, "worktrees", c.AttemptID)
	if err := a.prepareWorktree(ctx, wt, model.BranchName(c.RoutineName, c.AttemptID)); err != nil {
		t.Fatalf("prepareWorktree: %v", err)
	}
	m := a.manifest
	dir := filepath.Join(r.cfg.DataDir, "greenfield", c.AttemptID)
	if m == nil || m.Kind != manifestKindGreenfield || m.WorktreePath != dir || m.BaseBranch != "main" {
		t.Fatalf("manifest = %+v", m)
	}
	head, err := r.git.Run(ctx, dir, "rev-parse", "--verify", "HEAD^{commit}")
	if err != nil || strings.TrimSpace(head) != m.BaseCommit || !isFullCommit(m.BaseCommit) {
		t.Fatalf("init commit: head %q base %q err %v", strings.TrimSpace(head), m.BaseCommit, err)
	}
	// git_inspect works against the init commit like any other base.
	git, err := r.git.Inspect(ctx, dir, m.BaseCommit)
	if err != nil || git.Commits != 0 || git.Dirty {
		t.Fatalf("inspect = %+v %v", git, err)
	}

	// Success with a project_name: the directory moves to projects_root/<slug>.
	a.greenfieldMove(json.RawMessage(`{"project_path":".","project_name":"My App!"}`))
	dest := filepath.Join(projects, "my-app")
	if _, err := os.Stat(dest); err != nil {
		t.Fatalf("project not moved to %s: %v", dest, err)
	}
	if a.manifest.WorktreePath != dest {
		t.Errorf("manifest path = %q, want %q", a.manifest.WorktreePath, dest)
	}
	loaded, err := r.manifests.Load(c.AttemptID)
	if err != nil || loaded.WorktreePath != dest {
		t.Errorf("persisted manifest path = %+v %v", loaded, err)
	}

	// Cleanup keeps the project: the directory is the product.
	cl := a.cleanup(ctx, model.Succeeded, git, false)
	if cl.Outcome != "kept" || cl.Reason != "greenfield project" {
		t.Errorf("cleanup = %+v", cl)
	}
	if _, err := os.Stat(dest); err != nil {
		t.Errorf("cleanup removed the project: %v", err)
	}

	// A second project with the same name is refused and retained in place.
	c2 := greenfieldClaim()
	a2 := bareAttempt(r, c2)
	if err := a2.prepareWorktree(ctx, filepath.Join(r.cfg.DataDir, "worktrees", c2.AttemptID), model.BranchName(c2.RoutineName, c2.AttemptID)); err != nil {
		t.Fatalf("second prepare: %v", err)
	}
	inPlace := a2.manifest.WorktreePath
	a2.greenfieldMove(json.RawMessage(`{"project_name":"My App!"}`))
	if a2.manifest.WorktreePath != inPlace {
		t.Errorf("refused move should retain in place, got %q", a2.manifest.WorktreePath)
	}
	if _, err := os.Stat(inPlace); err != nil {
		t.Errorf("retained project missing: %v", err)
	}
	// An invalid slug is refused too.
	a2.greenfieldMove(json.RawMessage(`{"project_name":"!!!"}`))
	if a2.manifest.WorktreePath != inPlace {
		t.Errorf("invalid slug should retain in place, got %q", a2.manifest.WorktreePath)
	}
}

func TestSlugify(t *testing.T) {
	for in, want := range map[string]string{
		"My App!":     "my-app",
		"  Weather  ": "weather",
		"a b   c":     "a-b-c",
		"ALLCAPS42":   "allcaps42",
		"!!!":         "",
	} {
		if got := slugify(in); got != want {
			t.Errorf("slugify(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestPrepareWorktreeVerifyOfCutsAtSubjectHead(t *testing.T) {
	ctx := context.Background()
	gf := newGitFixture(t)
	// The subject attempt made a branch with one commit, like a real attempt.
	gf.run(t, gf.checkout, "checkout", "-q", "-b", "forge/demo-subject")
	gf.write(t, filepath.Join(gf.checkout, "sub.txt"), "subject work\n")
	gf.run(t, gf.checkout, "add", "sub.txt")
	gf.run(t, gf.checkout, "commit", "-q", "-m", "subject")
	head := gf.run(t, gf.checkout, "rev-parse", "HEAD")

	dataDir := filepath.Join(gf.root, "worker")
	if err := os.MkdirAll(filepath.Join(dataDir, "worktrees"), 0o700); err != nil {
		t.Fatal(err)
	}
	manifests, err := NewManifestStore(dataDir, testWorkerID, time.Now)
	if err != nil {
		t.Fatal(err)
	}
	r := &Runner{cfg: &Config{Daemon: "unix://x", DataDir: dataDir, MaxConcurrent: 1}, workerID: testWorkerID,
		git: gf.g, manifests: manifests, daemon: &fakeDaemon{}, repos: map[string]*Repository{"demo": gf.repo},
		log: slog.New(slog.DiscardHandler), clock: time.Now}
	c := &protocol.Claim{
		AttemptID: model.NewID(), TargetID: model.NewID(), WorkID: model.NewID(), RoutineName: "verify",
		Repository: "demo", Mode: "verify", TimeoutSeconds: 60, LeaseToken: "lease",
		VerifyOf: &protocol.VerifyOf{AttemptID: model.NewID(), Branch: "forge/demo-subject", Head: head},
	}
	a := bareAttempt(r, c)
	wt := filepath.Join(dataDir, "worktrees", c.AttemptID)
	if err := a.prepareWorktree(ctx, wt, model.BranchName(c.RoutineName, c.AttemptID)); err != nil {
		t.Fatalf("prepareWorktree: %v", err)
	}
	if got := gf.run(t, wt, "rev-parse", "HEAD"); got != head {
		t.Errorf("worktree HEAD = %s, want subject head %s", got, head)
	}
	if a.manifest.BaseBranch != "forge/demo-subject" || a.manifest.BaseCommit != head {
		t.Errorf("manifest base = %s@%s", a.manifest.BaseBranch, a.manifest.BaseCommit)
	}

	// A head that is not present fails prepare without fetching.
	c2 := &protocol.Claim{
		AttemptID: model.NewID(), TargetID: model.NewID(), WorkID: model.NewID(), RoutineName: "verify",
		Repository: "demo", Mode: "verify", TimeoutSeconds: 60,
		VerifyOf: &protocol.VerifyOf{AttemptID: model.NewID(), Branch: "forge/demo-subject", Head: strings.Repeat("0", 40)},
	}
	a2 := bareAttempt(r, c2)
	if err := a2.prepareWorktree(ctx, filepath.Join(dataDir, "worktrees", c2.AttemptID), model.BranchName("verify", c2.AttemptID)); err == nil || !strings.Contains(err.Error(), "not present") {
		t.Errorf("missing subject head: err = %v", err)
	}
}
