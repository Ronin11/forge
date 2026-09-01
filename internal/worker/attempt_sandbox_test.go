package worker

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
)

// TestAttemptRefusesSandboxRequiredWithoutBwrap is the worker-side belt and
// braces under DESIGN §19's routing rule: a require_sandbox claim on a
// sandbox-missing worker fails as prepare_failed instead of running naked.
func TestAttemptRefusesSandboxRequiredWithoutBwrap(t *testing.T) {
	f := newRunnerFixture(t, "inventory")
	f.runner.sandbox = nil // the fixture never sets one, but be explicit
	c := f.claim(model.AutonomyAuto)
	c.Policy.RequireSandbox = true
	req := f.run(c)
	if req.State != model.Failed || req.FailureReason != model.ReasonPrepareFailed {
		t.Fatalf("state = %s reason = %s, want failed/prepare_failed", req.State, req.FailureReason)
	}
}

// TestAttemptRunsFakeClaudeSandboxed drives a whole attempt — worktree, MCP
// config, fake-claude writing and committing, git_inspect, cleanup — with the
// executor wrapped in real bubblewrap, proving the launch-site integration
// leaves the supervisor's process handling intact.
func TestAttemptRunsFakeClaudeSandboxed(t *testing.T) {
	if _, err := exec.LookPath("bwrap"); err != nil {
		t.Skip("bwrap not installed")
	}
	f := newRunnerFixture(t, "commit")
	f.runner.sandbox = NewSandbox(filepath.Dir(f.dataDir), SandboxSettings{})
	if f.runner.sandbox == nil {
		t.Fatal("NewSandbox returned nil with bwrap present")
	}
	c := f.claim(model.AutonomyAuto)
	req := f.run(c)
	if req.State != model.Succeeded {
		t.Fatalf("state = %s (%s), exit %d\nstderr-ish result: %s", req.State, req.FailureReason, req.ExitCode, req.ResultText)
	}
	if req.Git.Commits != 1 || req.Git.Dirty {
		t.Errorf("git = %+v, want one commit in a clean worktree", req.Git)
	}
	var sandboxed, denied bool
	f.daemon.mu.Lock()
	for _, e := range f.daemon.events {
		switch {
		case e.Kind == protocol.KindLifecycle && e.Message == "sandboxed":
			sandboxed = true
		case e.Kind == protocol.KindLifecycle && e.Message == "net.denied":
			denied = true
		}
	}
	f.daemon.mu.Unlock()
	if !sandboxed {
		t.Error("no 'sandboxed' lifecycle event on the timeline")
	}
	if denied {
		t.Error("unexpected net.denied event: the fixture makes no network calls")
	}
}

// TestAttemptCommitFilesLeavesDirtyWorktreeRetained pairs the commit-files
// fixture with the retention rules: uncommitted writes make git_inspect report
// dirty and DecideCleanup retain the worktree.
func TestAttemptCommitFilesLeavesDirtyWorktreeRetained(t *testing.T) {
	f := newRunnerFixture(t, "commit-files")
	c := f.claim(model.AutonomyAuto)
	req := f.run(c)
	if req.State != model.Succeeded {
		t.Fatalf("state = %s (%s)", req.State, req.FailureReason)
	}
	if !req.Git.Dirty || req.Git.Commits != 0 {
		t.Errorf("git = %+v, want dirty with no commits", req.Git)
	}
	// Untracked entries surface as the file and the collapsed directory.
	wantPaths := map[string]bool{"FORGE_NOTES.md": true, "docs/": true}
	for _, p := range req.Git.ChangedPaths {
		delete(wantPaths, p)
	}
	if len(wantPaths) != 0 {
		t.Errorf("changed paths %v missing %v", req.Git.ChangedPaths, wantPaths)
	}
	if req.Cleanup.Outcome != "retained" || !strings.Contains(req.Cleanup.Command, "forge cleanup") {
		t.Errorf("cleanup = %+v, want retained with a cleanup command", req.Cleanup)
	}
	m, err := f.runner.manifests.Load(c.AttemptID)
	if err != nil || m.Lifecycle != ManifestRetained {
		t.Errorf("manifest = %+v, %v", m, err)
	}
	for p := range map[string]string{"FORGE_NOTES.md": "", "docs/PLAN.md": ""} {
		if _, err := os.Stat(filepath.Join(m.WorktreePath, p)); err != nil {
			t.Errorf("retained worktree is missing %s: %v", p, err)
		}
	}
}
