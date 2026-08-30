package worker

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

// mustGit runs git in dir and fails the test on error.
func mustGit(t *testing.T, dir string, args ...string) string {
	t.Helper()
	cmd := exec.Command("git", args...)
	cmd.Dir = dir
	cmd.Env = append(os.Environ(),
		"GIT_AUTHOR_NAME=t", "GIT_AUTHOR_EMAIL=t@t", "GIT_COMMITTER_NAME=t", "GIT_COMMITTER_EMAIL=t@t")
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git %s: %v\n%s", strings.Join(args, " "), err, out)
	}
	return strings.TrimSpace(string(out))
}

// newTestRepo creates a bare origin and a checkout of it with one commit on main.
func newTestRepo(t *testing.T) (checkout, origin string) {
	t.Helper()
	root := t.TempDir()
	origin = filepath.Join(root, "origin.git")
	checkout = filepath.Join(root, "checkout")
	mustGit(t, root, "init", "-q", "--bare", "-b", "main", origin)
	mustGit(t, root, "clone", "-q", origin, checkout)
	if err := os.WriteFile(filepath.Join(checkout, "README"), []byte("hi\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	mustGit(t, checkout, "add", "README")
	mustGit(t, checkout, "commit", "-qm", "init")
	mustGit(t, checkout, "push", "-q", "origin", "main")
	return checkout, origin
}

func TestValidateRepository(t *testing.T) {
	ctx := context.Background()
	checkout, origin := newTestRepo(t)
	repo, err := ValidateRepository(ctx, "r", checkout, "")
	if err != nil {
		t.Fatal(err)
	}
	if repo.BaseBranch != "main" {
		t.Errorf("base branch %q", repo.BaseBranch)
	}
	if !strings.HasPrefix(repo.RemoteIdentity, "file://") {
		t.Errorf("identity %q", repo.RemoteIdentity)
	}
	if _, err := ValidateRepository(ctx, "r", checkout, "nope"); err == nil {
		t.Error("expected unresolvable base branch error")
	}
	if _, err := ValidateRepository(ctx, "bare", origin, ""); err == nil {
		t.Error("expected bare repository to be rejected")
	}
	if _, err := ValidateRepository(ctx, "sub", filepath.Join(checkout, "sub"), ""); err == nil {
		t.Error("expected missing path to be rejected")
	}
	noOrigin := t.TempDir()
	mustGit(t, noOrigin, "init", "-q")
	if _, err := ValidateRepository(ctx, "no", noOrigin, ""); err == nil {
		t.Error("expected missing origin to be rejected")
	}
}

func TestNormalizeRemoteIdentity(t *testing.T) {
	cases := map[string]string{
		"git@github.com:Ronin11/equitizr.git":          "github.com/Ronin11/equitizr",
		"https://github.com/Ronin11/ronin11.github.io": "github.com/Ronin11/ronin11.github.io",
		"ssh://git@github.com/o/r.git":                 "github.com/o/r",
	}
	for in, want := range cases {
		got, err := normalizeRemoteIdentity(in, "/")
		if err != nil || got != want {
			t.Errorf("%s: got %q, %v want %q", in, got, err, want)
		}
	}
	if _, err := normalizeRemoteIdentity("", "/"); err == nil {
		t.Error("empty remote accepted")
	}
}

func TestFetchBaseDoesNotTouchCheckout(t *testing.T) {
	ctx := context.Background()
	checkout, origin := newTestRepo(t)
	// Advance origin from a second clone; leave the checkout dirty and on a topic branch.
	other := filepath.Join(t.TempDir(), "other")
	mustGit(t, checkout, "clone", "-q", origin, other)
	if err := os.WriteFile(filepath.Join(other, "B"), []byte("b"), 0o644); err != nil {
		t.Fatal(err)
	}
	mustGit(t, other, "add", "B")
	mustGit(t, other, "commit", "-qm", "second")
	mustGit(t, other, "push", "-q", "origin", "main")
	want := mustGit(t, other, "rev-parse", "HEAD")

	mustGit(t, checkout, "checkout", "-qb", "topic")
	if err := os.WriteFile(filepath.Join(checkout, "README"), []byte("dirty\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	before := mustGit(t, checkout, "status", "--porcelain")

	repo, err := ValidateRepository(ctx, "r", checkout, "main")
	if err != nil {
		t.Fatal(err)
	}
	commit, err := repo.FetchBase(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if commit != want {
		t.Errorf("resolved %s want %s", commit, want)
	}
	if got := mustGit(t, checkout, "rev-parse", "--abbrev-ref", "HEAD"); got != "topic" {
		t.Errorf("checkout branch changed to %s", got)
	}
	if got := mustGit(t, checkout, "status", "--porcelain"); got != before {
		t.Errorf("checkout status changed: %q -> %q", before, got)
	}
	// Origin change is detected.
	mustGit(t, checkout, "remote", "set-url", "origin", "https://example.com/x/y.git")
	if _, err := repo.FetchBase(ctx); err == nil || !strings.Contains(err.Error(), "origin changed") {
		t.Errorf("expected origin change error, got %v", err)
	}
}

func TestCleanupMatrix(t *testing.T) {
	ctx := context.Background()
	type setup func(t *testing.T, wt, origin string)
	commit := func(t *testing.T, wt string) {
		if err := os.WriteFile(filepath.Join(wt, "NEW"), []byte("x"), 0o644); err != nil {
			t.Fatal(err)
		}
		mustGit(t, wt, "add", "NEW")
		mustGit(t, wt, "commit", "-qm", "new")
	}
	cases := []struct {
		name       string
		setup      setup
		wantRemove bool
		wantReason string
	}{
		{"clean", func(*testing.T, string, string) {}, true, "clean"},
		{"dirty", func(t *testing.T, wt, _ string) {
			if err := os.WriteFile(filepath.Join(wt, "README"), []byte("changed"), 0o644); err != nil {
				t.Fatal(err)
			}
		}, false, "dirty worktree"},
		{"untracked", func(t *testing.T, wt, _ string) {
			if err := os.WriteFile(filepath.Join(wt, "junk"), []byte("x"), 0o644); err != nil {
				t.Fatal(err)
			}
		}, false, "dirty worktree"},
		{"unpushed", func(t *testing.T, wt, _ string) { commit(t, wt) }, false, "unpushed commits"},
		{"pushed", func(t *testing.T, wt, _ string) {
			commit(t, wt)
			mustGit(t, wt, "push", "-q", "origin", "HEAD")
		}, true, "clean"},
		{"missing", func(t *testing.T, wt, _ string) {
			if err := os.RemoveAll(wt); err != nil {
				t.Fatal(err)
			}
		}, false, "worktree missing"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			checkout, origin := newTestRepo(t)
			repo, err := ValidateRepository(ctx, "r", checkout, "")
			if err != nil {
				t.Fatal(err)
			}
			base, err := repo.FetchBase(ctx)
			if err != nil {
				t.Fatal(err)
			}
			wt := filepath.Join(t.TempDir(), "worktrees", "r", "attempt1")
			if err := repo.AddWorktree(ctx, wt, "forge/test-"+c.name, base); err != nil {
				t.Fatal(err)
			}
			c.setup(t, wt, origin)
			state, err := repo.InspectWorktree(ctx, wt, base)
			if err != nil {
				t.Fatal(err)
			}
			remove, reason := DecideCleanup(state)
			if remove != c.wantRemove || reason != c.wantReason {
				t.Fatalf("got remove=%v reason=%q want %v %q (state %+v)", remove, reason, c.wantRemove, c.wantReason, state)
			}
			if remove {
				if err := repo.RemoveWorktree(ctx, wt, false); err != nil {
					t.Fatal(err)
				}
				if _, err := os.Stat(wt); !os.IsNotExist(err) {
					t.Error("worktree still present")
				}
				// The branch must survive.
				mustGit(t, checkout, "rev-parse", "--verify", "refs/heads/forge/test-"+c.name)
			} else if state.Exists {
				// Non-forced removal must refuse dirty/unpushed trees; forced must work and keep the branch.
				if c.name == "dirty" || c.name == "untracked" {
					if err := repo.RemoveWorktree(ctx, wt, false); err == nil {
						t.Error("non-forced removal of a dirty worktree succeeded")
					}
				}
				if err := repo.RemoveWorktree(ctx, wt, true); err != nil {
					t.Fatal(err)
				}
				mustGit(t, checkout, "rev-parse", "--verify", "refs/heads/forge/test-"+c.name)
			}
			// The registered checkout is untouched.
			if got := mustGit(t, checkout, "status", "--porcelain"); got != "" {
				t.Errorf("checkout dirty: %q", got)
			}
			if got := mustGit(t, checkout, "rev-parse", "--abbrev-ref", "HEAD"); got != "main" {
				t.Errorf("checkout branch %q", got)
			}
		})
	}
}
