package worker

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"
)

// gitFixture is a bare origin with one clone, both under t.TempDir(), so every
// test runs against a real repository and leaves nothing behind.
type gitFixture struct {
	g        Git
	root     string
	origin   string
	checkout string
	repo     *Repository
}

// newGitFixture builds origin.git and checkout with one commit on master pushed.
// GIT_CONFIG_GLOBAL points at a private file so commits work on any machine and
// the developer's own config (signing, hooks) cannot leak in.
func newGitFixture(t *testing.T) *gitFixture {
	t.Helper()
	root := t.TempDir()
	cfg := filepath.Join(root, "gitconfig")
	if err := os.WriteFile(cfg, []byte("[user]\n\tname = Forge Test\n\temail = forge@example.invalid\n[init]\n\tdefaultBranch = master\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv("GIT_CONFIG_GLOBAL", cfg)
	t.Setenv("GIT_CONFIG_NOSYSTEM", "1")

	f := &gitFixture{root: root, origin: filepath.Join(root, "origin.git"), checkout: filepath.Join(root, "checkout")}
	f.run(t, root, "init", "--bare", "-b", "master", f.origin)
	f.run(t, root, "clone", f.origin, f.checkout)
	f.write(t, filepath.Join(f.checkout, "README.md"), "hello\n")
	f.run(t, f.checkout, "add", "README.md")
	f.run(t, f.checkout, "commit", "-q", "-m", "initial")
	f.run(t, f.checkout, "push", "-q", "-u", "origin", "master")

	repo, err := f.g.ValidateRepository(context.Background(), "demo", f.checkout, "master")
	if err != nil {
		t.Fatalf("ValidateRepository: %v", err)
	}
	f.repo = repo
	return f
}

func (f *gitFixture) run(t *testing.T, dir string, args ...string) string {
	t.Helper()
	out, err := f.g.Run(context.Background(), dir, args...)
	if err != nil {
		t.Fatalf("git %s in %s: %v", strings.Join(args, " "), dir, err)
	}
	return strings.TrimSpace(out)
}

func (f *gitFixture) write(t *testing.T, path, content string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
}

// pushFromSecondClone advances origin/master from a different clone so the
// checkout's remote-tracking ref becomes stale.
func (f *gitFixture) pushFromSecondClone(t *testing.T) string {
	t.Helper()
	other := filepath.Join(f.root, "other")
	f.run(t, f.root, "clone", "-q", f.origin, other)
	f.write(t, filepath.Join(other, "other.txt"), "from other\n")
	f.run(t, other, "add", "other.txt")
	f.run(t, other, "commit", "-q", "-m", "other")
	f.run(t, other, "push", "-q", "origin", "master")
	return f.run(t, other, "rev-parse", "HEAD")
}

// worktreeRoot is a real directory to hold attempt worktrees, like data_dir/worktrees.
func (f *gitFixture) worktreeRoot(t *testing.T) string {
	t.Helper()
	dir := filepath.Join(f.root, "worktrees")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	return dir
}

func TestValidateRepository(t *testing.T) {
	f := newGitFixture(t)
	ctx := context.Background()

	t.Run("good", func(t *testing.T) {
		canonical, err := filepath.EvalSymlinks(f.checkout)
		if err != nil {
			t.Fatal(err)
		}
		if f.repo.Path != canonical {
			t.Errorf("Path = %q, want canonical %q", f.repo.Path, canonical)
		}
		wantOrigin, err := filepath.EvalSymlinks(f.origin)
		if err != nil {
			t.Fatal(err)
		}
		if f.repo.OriginIdentity != "file://"+wantOrigin {
			t.Errorf("OriginIdentity = %q, want %q", f.repo.OriginIdentity, "file://"+wantOrigin)
		}
		if f.repo.OriginURL == "" || f.repo.BaseBranch != "master" || f.repo.Name != "demo" {
			t.Errorf("unexpected repository %+v", f.repo)
		}
	})

	t.Run("bare rejected", func(t *testing.T) {
		_, err := f.g.ValidateRepository(ctx, "bare", f.origin, "")
		if err == nil || !strings.Contains(err.Error(), "not a Git worktree") || !strings.Contains(err.Error(), "repository bare") {
			t.Errorf("want bare rejection naming the repository, got %v", err)
		}
	})

	t.Run("no origin rejected", func(t *testing.T) {
		dir := filepath.Join(f.root, "no-origin")
		f.run(t, f.root, "init", "-q", "-b", "master", dir)
		_, err := f.g.ValidateRepository(ctx, "lonely", dir, "")
		if err == nil || !strings.Contains(err.Error(), "origin") || !strings.Contains(err.Error(), "repository lonely") {
			t.Errorf("want origin error naming the repository, got %v", err)
		}
	})

	t.Run("bad base_branch rejected", func(t *testing.T) {
		for _, bad := range []string{"HEAD", "refs/heads/master", "origin/master", "bad..name", "-x", "space name"} {
			_, err := f.g.ValidateRepository(ctx, "demo", f.checkout, bad)
			if err == nil || !strings.Contains(err.Error(), "base_branch") {
				t.Errorf("base_branch %q: want rejection, got %v", bad, err)
			}
		}
		if _, err := f.g.ValidateRepository(ctx, "demo", f.checkout, "release/2026.07"); err != nil {
			t.Errorf("release/2026.07 should be a valid short name: %v", err)
		}
	})

	t.Run("not a directory rejected", func(t *testing.T) {
		_, err := f.g.ValidateRepository(ctx, "file", filepath.Join(f.checkout, "README.md"), "")
		if err == nil || !strings.Contains(err.Error(), "not a directory") {
			t.Errorf("want not-a-directory error, got %v", err)
		}
	})

	t.Run("symlinked path canonicalised", func(t *testing.T) {
		link := filepath.Join(f.root, "link")
		if err := os.Symlink(f.checkout, link); err != nil {
			t.Fatal(err)
		}
		repo, err := f.g.ValidateRepository(ctx, "linked", link, "")
		if err != nil {
			t.Fatal(err)
		}
		if repo.Path != f.repo.Path {
			t.Errorf("Path = %q, want canonical %q", repo.Path, f.repo.Path)
		}
		if !SameIdentity(repo.OriginIdentity, f.repo.OriginIdentity) {
			t.Errorf("identity %q differs from %q", repo.OriginIdentity, f.repo.OriginIdentity)
		}
	})
}

func TestNormalizeRemoteIdentity(t *testing.T) {
	root := t.TempDir()
	repoPath := filepath.Join(root, "repo")
	bare := filepath.Join(root, "bare.git")
	for _, dir := range []string{repoPath, bare} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			t.Fatal(err)
		}
	}
	canonicalBare, err := filepath.EvalSymlinks(bare)
	if err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(root, "bare-link.git")
	if err := os.Symlink(bare, link); err != nil {
		t.Fatal(err)
	}

	cases := []struct {
		remote, want string
		wantErr      bool
	}{
		{remote: "git@github.com:Ronin11/equitizr.git", want: "github.com/Ronin11/equitizr"},
		{remote: "https://github.com/Ronin11/ronin11.github.io", want: "github.com/Ronin11/ronin11.github.io"},
		{remote: "https://GitHub.com/Ronin11/ronin11.github.io.git/", want: "github.com/Ronin11/ronin11.github.io"},
		{remote: "ssh://git@host:2222/a/b.git", want: "host:2222/a/b"},
		{remote: "ssh://git@Host.Example/a/B.git", want: "host.example/a/B"},
		{remote: "file://" + bare, want: "file://" + canonicalBare},
		{remote: "file://" + link, want: "file://" + canonicalBare},
		{remote: "../bare.git", want: "file://" + canonicalBare},
		{remote: bare, want: "file://" + canonicalBare},
		{remote: "", wantErr: true},
		{remote: "git@github.com:", wantErr: true},
		{remote: "https://github.com", wantErr: true},
		{remote: "../does-not-exist.git", wantErr: true},
	}
	for _, tc := range cases {
		got, err := NormalizeRemoteIdentity(tc.remote, repoPath)
		if tc.wantErr {
			if err == nil {
				t.Errorf("%q: want error, got %q", tc.remote, got)
			}
			continue
		}
		if err != nil {
			t.Errorf("%q: %v", tc.remote, err)
			continue
		}
		if got != tc.want {
			t.Errorf("%q: got %q, want %q", tc.remote, got, tc.want)
		}
	}
}

func TestSameIdentity(t *testing.T) {
	cases := []struct {
		a, b string
		want bool
	}{
		{"github.com/Ronin11/equitizr", "github.com/ronin11/Equitizr", true},
		{"github.com/Ronin11/equitizr", "github.com/Ronin11/factory", false},
		{"gitlab.com/Ronin11/equitizr", "gitlab.com/ronin11/equitizr", false},
		{"gitlab.com/Ronin11/equitizr", "gitlab.com/Ronin11/equitizr", true},
		{"file:///tmp/A", "file:///tmp/a", false},
		{"github.com/Ronin11/equitizr", "gitlab.com/Ronin11/equitizr", false},
	}
	for _, tc := range cases {
		if got := SameIdentity(tc.a, tc.b); got != tc.want {
			t.Errorf("SameIdentity(%q, %q) = %v, want %v", tc.a, tc.b, got, tc.want)
		}
	}
}

func TestCheckOrigin(t *testing.T) {
	f := newGitFixture(t)
	ctx := context.Background()
	if err := f.g.CheckOrigin(ctx, f.repo); err != nil {
		t.Fatalf("unchanged origin: %v", err)
	}
	f.run(t, f.checkout, "remote", "set-url", "origin", "git@github.com:Ronin11/equitizr.git")
	err := f.g.CheckOrigin(ctx, f.repo)
	if err == nil || !strings.Contains(err.Error(), "origin changed") {
		t.Errorf("want origin-changed error, got %v", err)
	}
}

func TestFetch(t *testing.T) {
	f := newGitFixture(t)
	ctx := context.Background()

	want := f.pushFromSecondClone(t)
	fetched, warn, err := f.g.Fetch(ctx, f.repo, "master")
	if err != nil || warn != nil || !fetched {
		t.Fatalf("Fetch = (%v, %v, %v), want (true, nil, nil)", fetched, warn, err)
	}
	if got := f.run(t, f.checkout, "rev-parse", "refs/remotes/origin/master"); got != want {
		t.Errorf("origin/master = %s, want %s", got, want)
	}
	if got := f.run(t, f.checkout, "rev-parse", "HEAD"); got == want {
		t.Errorf("fetch moved the checkout's HEAD to %s", got)
	}

	// With origin gone the fetch fails, but the ref exists: proceed with a warning.
	gone := f.origin + ".gone"
	if err := os.Rename(f.origin, gone); err != nil {
		t.Fatal(err)
	}
	fetched, warn, err = f.g.Fetch(ctx, f.repo, "master")
	if err != nil || fetched {
		t.Fatalf("Fetch with origin missing = (%v, %v, %v), want (false, warn, nil)", fetched, warn, err)
	}
	if warn == nil || !strings.Contains(warn.Error(), "fetch origin/master for demo") {
		t.Errorf("warn = %v, want fetch error naming the repository", warn)
	}

	// No remote-tracking ref to fall back on: fatal.
	fetched, warn, err = f.g.Fetch(ctx, f.repo, "never")
	if err == nil || fetched || warn != nil {
		t.Fatalf("Fetch of unknown branch = (%v, %v, %v), want err", fetched, warn, err)
	}
	if !strings.Contains(err.Error(), "refs/remotes/origin/never does not exist locally") {
		t.Errorf("err = %v, want mention of the missing local ref", err)
	}
	if err := os.Rename(gone, f.origin); err != nil {
		t.Fatal(err)
	}

	// A malformed base never reaches git.
	if _, _, err := f.g.Fetch(ctx, f.repo, "origin/master"); err == nil {
		t.Error("Fetch accepted origin/master as a base branch")
	}
}

func TestResolveBase(t *testing.T) {
	f := newGitFixture(t)
	ctx := context.Background()
	want := f.run(t, f.checkout, "rev-parse", "refs/remotes/origin/master")

	unconfigured := *f.repo
	unconfigured.BaseBranch = ""

	_, _, err := f.g.ResolveBase(ctx, &unconfigured, "")
	if err == nil || !strings.Contains(err.Error(), "set base_branch for demo") {
		t.Errorf("without any base: want 'set base_branch for demo', got %v", err)
	}

	branch, commit, err := f.g.ResolveBase(ctx, &unconfigured, "master")
	if err != nil || branch != "master" || commit != want {
		t.Errorf("forge.toml base: (%q, %q, %v), want (master, %s, nil)", branch, commit, err, want)
	}
	if _, _, err := f.g.ResolveBase(ctx, &unconfigured, "missing"); err == nil {
		t.Error("forge.toml base naming a branch origin lacks should fail")
	}

	f.run(t, f.checkout, "remote", "set-head", "origin", "master")
	branch, commit, err = f.g.ResolveBase(ctx, &unconfigured, "")
	if err != nil || branch != "master" || commit != want {
		t.Errorf("origin/HEAD base: (%q, %q, %v), want (master, %s, nil)", branch, commit, err, want)
	}

	branch, commit, err = f.g.ResolveBase(ctx, f.repo, "ignored")
	if err != nil || branch != "master" || commit != want {
		t.Errorf("configured base: (%q, %q, %v), want (master, %s, nil)", branch, commit, err, want)
	}
}

// checkoutSnapshot captures everything WorktreeAdd must leave alone.
func (f *gitFixture) checkoutSnapshot(t *testing.T) string {
	t.Helper()
	return strings.Join([]string{
		f.run(t, f.checkout, "symbolic-ref", "HEAD"),
		f.run(t, f.checkout, "rev-parse", "HEAD"),
		f.run(t, f.checkout, "status", "--porcelain"),
		f.run(t, f.checkout, "ls-files", "-s"),
	}, "\n---\n")
}

func TestWorktreeAdd(t *testing.T) {
	f := newGitFixture(t)
	ctx := context.Background()
	root := f.worktreeRoot(t)
	_, commit, err := f.g.ResolveBase(ctx, f.repo, "")
	if err != nil {
		t.Fatal(err)
	}

	// A dirty checkout: modified tracked file and an untracked file.
	f.write(t, filepath.Join(f.checkout, "README.md"), "edited locally\n")
	f.write(t, filepath.Join(f.checkout, "scratch.txt"), "untracked\n")
	before := f.checkoutSnapshot(t)
	if !strings.Contains(before, "M README.md") || !strings.Contains(before, "?? scratch.txt") {
		t.Fatalf("fixture is not dirty as intended:\n%s", before)
	}

	path := filepath.Join(root, "attempt1")
	state, err := f.g.WorktreeState(ctx, f.repo, path)
	if err != nil || state.PathExists || state.Registered {
		t.Fatalf("state before add = %+v, %v", state, err)
	}
	if err := f.g.WorktreeAdd(ctx, f.repo, path, "forge/demo-attempt1", commit); err != nil {
		t.Fatalf("WorktreeAdd: %v", err)
	}
	if after := f.checkoutSnapshot(t); after != before {
		t.Errorf("WorktreeAdd changed the checkout:\nbefore:\n%s\nafter:\n%s", before, after)
	}
	state, err = f.g.WorktreeState(ctx, f.repo, path)
	if err != nil {
		t.Fatal(err)
	}
	if !state.PathExists || !state.Registered || state.Branch != "forge/demo-attempt1" || state.Head != commit {
		t.Errorf("state after add = %+v", state)
	}
	if got := f.run(t, f.checkout, "rev-parse", "--verify", "refs/heads/forge/demo-attempt1"); got != commit {
		t.Errorf("branch tip = %s, want %s", got, commit)
	}
	if _, err := os.Stat(filepath.Join(path, "README.md")); err != nil {
		t.Errorf("worktree has no README.md: %v", err)
	}

	// Pre-checks: existing path, symlinked parent, bad commit, relative path.
	if err := f.g.WorktreeAdd(ctx, f.repo, path, "forge/again", commit); err == nil || !strings.Contains(err.Error(), "already exists") {
		t.Errorf("existing path: want refusal, got %v", err)
	}
	link := filepath.Join(f.root, "worktrees-link")
	if err := os.Symlink(root, link); err != nil {
		t.Fatal(err)
	}
	if err := f.g.WorktreeAdd(ctx, f.repo, filepath.Join(link, "attempt2"), "forge/attempt2", commit); err == nil || !strings.Contains(err.Error(), "symlink") {
		t.Errorf("symlinked parent: want refusal, got %v", err)
	}
	if err := f.g.WorktreeAdd(ctx, f.repo, filepath.Join(root, "attempt3"), "forge/attempt3", commit[:12]); err == nil {
		t.Error("abbreviated commit: want refusal")
	}
	if err := f.g.WorktreeAdd(ctx, f.repo, "relative/attempt4", "forge/attempt4", commit); err == nil {
		t.Error("relative path: want refusal")
	}
	if _, err := os.Lstat(filepath.Join(root, "attempt3")); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("refused add left a directory behind: %v", err)
	}
}

func TestWorktreeStateDuplicateListing(t *testing.T) {
	entries := parseWorktreeList("worktree /a\x00HEAD 0123\x00branch refs/heads/main\x00\x00worktree /b\x00HEAD 4567\x00detached\x00\x00")
	if len(entries) != 2 || entries[0].Path != "/a" || entries[0].Branch != "main" || entries[0].Head != "0123" || entries[1].Path != "/b" || entries[1].Branch != "" {
		t.Errorf("parseWorktreeList = %+v", entries)
	}
}

func TestInspect(t *testing.T) {
	f := newGitFixture(t)
	ctx := context.Background()
	root := f.worktreeRoot(t)
	_, base, err := f.g.ResolveBase(ctx, f.repo, "")
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(root, "attempt")
	if err := f.g.WorktreeAdd(ctx, f.repo, path, "forge/inspect", base); err != nil {
		t.Fatal(err)
	}

	got, err := f.g.Inspect(ctx, path, base)
	if err != nil {
		t.Fatal(err)
	}
	if got.Dirty || got.Commits != 0 || got.FilesChanged != 0 || !got.Pushed || got.Head != base || len(got.ChangedPaths) != 0 {
		t.Errorf("clean: %+v", got)
	}

	f.write(t, filepath.Join(path, "notes.txt"), "scratch\n")
	got, err = f.g.Inspect(ctx, path, base)
	if err != nil {
		t.Fatal(err)
	}
	if !got.Dirty || got.Commits != 0 || !got.Pushed || !slices.Equal(got.ChangedPaths, []string{"notes.txt"}) {
		t.Errorf("dirty: %+v", got)
	}

	f.write(t, filepath.Join(path, "README.md"), "hello\nworld\n")
	f.write(t, filepath.Join(path, "bin.dat"), "\x00\x01\x02\xff")
	f.run(t, path, "add", "README.md", "bin.dat")
	f.run(t, path, "commit", "-q", "-m", "work")
	head := f.run(t, path, "rev-parse", "HEAD")
	got, err = f.g.Inspect(ctx, path, base)
	if err != nil {
		t.Fatal(err)
	}
	if !got.Dirty || got.Commits != 1 || got.Pushed || got.Head != head {
		t.Errorf("committed, unpushed, still dirty: %+v", got)
	}
	if got.FilesChanged != 2 || got.Insertions != 1 || got.Deletions != 0 {
		t.Errorf("numstat: files=%d ins=%d del=%d, want 2/1/0", got.FilesChanged, got.Insertions, got.Deletions)
	}
	if !slices.Equal(got.ChangedPaths, []string{"README.md", "bin.dat", "notes.txt"}) {
		t.Errorf("ChangedPaths = %v", got.ChangedPaths)
	}
	if got.Head != f.run(t, path, "rev-parse", "HEAD") || f.run(t, f.checkout, "rev-parse", "HEAD") == head {
		t.Error("the checkout's HEAD moved with the worktree's commit")
	}

	if err := os.Remove(filepath.Join(path, "notes.txt")); err != nil {
		t.Fatal(err)
	}
	f.run(t, path, "push", "-q", "origin", "forge/inspect")
	got, err = f.g.Inspect(ctx, path, base)
	if err != nil {
		t.Fatal(err)
	}
	if got.Dirty || got.Commits != 1 || !got.Pushed || !slices.Equal(got.ChangedPaths, []string{"README.md", "bin.dat"}) {
		t.Errorf("pushed: %+v", got)
	}

	// Base given as a ref name works too, and an unknown base is an error.
	if got2, err := f.g.Inspect(ctx, path, "origin/master"); err != nil || got2.Commits != got.Commits || got2.Head != got.Head || !slices.Equal(got2.ChangedPaths, got.ChangedPaths) {
		t.Errorf("Inspect with ref base: %+v, %v", got2, err)
	}
	if _, err := f.g.Inspect(ctx, path, "no-such-ref"); err == nil {
		t.Error("Inspect with unknown base should fail")
	}
}

func TestParsers(t *testing.T) {
	files, ins, del, err := parseNumstat("3\t1\ta.go\n-\t-\tbin.dat\n0\t2\tsrc/{old => new}.go\n")
	if err != nil || files != 3 || ins != 3 || del != 3 {
		t.Errorf("parseNumstat = %d/%d/%d, %v", files, ins, del, err)
	}
	if _, _, _, err := parseNumstat("garbage\n"); err == nil {
		t.Error("parseNumstat accepted garbage")
	}
	got := parseNameStatusZ("M\x00a.go\x00R100\x00old.go\x00new.go\x00A\x00b.go\x00")
	if !slices.Equal(got, []string{"a.go", "old.go", "new.go", "b.go"}) {
		t.Errorf("parseNameStatusZ = %v", got)
	}
	got = parseStatusZ(" M a.go\x00?? new.txt\x00R  renamed.go\x00orig.go\x00")
	if !slices.Equal(got, []string{"a.go", "new.txt", "renamed.go", "orig.go"}) {
		t.Errorf("parseStatusZ = %v", got)
	}
}

func TestWorktreeRemove(t *testing.T) {
	f := newGitFixture(t)
	ctx := context.Background()
	root := f.worktreeRoot(t)
	_, base, err := f.g.ResolveBase(ctx, f.repo, "")
	if err != nil {
		t.Fatal(err)
	}

	dirty := filepath.Join(root, "dirty")
	if err := f.g.WorktreeAdd(ctx, f.repo, dirty, "forge/dirty", base); err != nil {
		t.Fatal(err)
	}
	f.write(t, filepath.Join(dirty, "junk.txt"), "unsaved\n")
	if err := f.g.WorktreeRemove(ctx, f.repo, dirty, false); err == nil {
		t.Fatal("removing a dirty worktree without force succeeded")
	}
	state, err := f.g.WorktreeState(ctx, f.repo, dirty)
	if err != nil || !state.PathExists || !state.Registered {
		t.Fatalf("dirty worktree after refused removal: %+v, %v", state, err)
	}
	if err := f.g.WorktreeRemove(ctx, f.repo, dirty, true); err != nil {
		t.Fatalf("forced removal: %v", err)
	}
	state, err = f.g.WorktreeState(ctx, f.repo, dirty)
	if err != nil || state.PathExists || state.Registered {
		t.Errorf("after forced removal: %+v, %v", state, err)
	}
	// The branch survives removal (DESIGN §6: Forge never deletes branches).
	if got := f.run(t, f.checkout, "rev-parse", "--verify", "refs/heads/forge/dirty"); got != base {
		t.Errorf("branch forge/dirty = %s after removal, want %s", got, base)
	}

	clean := filepath.Join(root, "clean")
	if err := f.g.WorktreeAdd(ctx, f.repo, clean, "forge/clean", base); err != nil {
		t.Fatal(err)
	}
	if err := f.g.WorktreeRemove(ctx, f.repo, clean, false); err != nil {
		t.Fatalf("removing a clean worktree: %v", err)
	}
	if _, err := os.Lstat(clean); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("clean worktree path remains: %v", err)
	}
	if err := f.g.WorktreeRemove(ctx, f.repo, clean, false); err == nil {
		t.Error("removing an already-removed worktree should fail")
	}
}

func TestGitConfigEnv(t *testing.T) {
	if got := GitConfigEnv(nil); got != nil {
		t.Errorf("GitConfigEnv(nil) = %v, want nil", got)
	}
	got := GitConfigEnv(map[string]string{
		"rerere.enabled":        "true",
		"merge.conflictstyle":   "zdiff3",
		"merge.mergiraf.name":   "mergiraf",
		"merge.mergiraf.driver": "mergiraf merge --git %O %A %B -s %S -x %X -y %Y -p %P -l %L",
	})
	want := []string{
		"GIT_CONFIG_COUNT=4",
		"GIT_CONFIG_KEY_0=merge.conflictstyle",
		"GIT_CONFIG_VALUE_0=zdiff3",
		"GIT_CONFIG_KEY_1=merge.mergiraf.driver",
		"GIT_CONFIG_VALUE_1=mergiraf merge --git %O %A %B -s %S -x %X -y %Y -p %P -l %L",
		"GIT_CONFIG_KEY_2=merge.mergiraf.name",
		"GIT_CONFIG_VALUE_2=mergiraf",
		"GIT_CONFIG_KEY_3=rerere.enabled",
		"GIT_CONFIG_VALUE_3=true",
	}
	if !slices.Equal(got, want) {
		t.Errorf("GitConfigEnv =\n%s\nwant\n%s", strings.Join(got, "\n"), strings.Join(want, "\n"))
	}
}

func TestRunErrorsAndTimeout(t *testing.T) {
	f := newGitFixture(t)
	ctx := context.Background()

	_, err := f.g.Run(ctx, f.checkout, "rev-parse", "--verify", "refs/heads/nope")
	if err == nil || !strings.HasPrefix(err.Error(), "git rev-parse: exit status") || !strings.Contains(err.Error(), "fatal") {
		t.Errorf("error format: %v", err)
	}
	if _, err := f.g.Run(ctx, f.checkout); err == nil {
		t.Error("Run with no arguments should fail")
	}

	// A git alias that sleeps stands in for a hung credential helper: the whole
	// process group must die at the timeout, not linger holding the pipes.
	slow := Git{Timeout: 300 * time.Millisecond}
	start := time.Now()
	_, err = slow.Run(ctx, f.checkout, "-c", "alias.hang=!sleep 30", "hang")
	elapsed := time.Since(start)
	if !errors.Is(err, context.DeadlineExceeded) || !strings.Contains(err.Error(), "timed out after 300ms") {
		t.Errorf("timeout error: %v", err)
	}
	if elapsed > 3*time.Second {
		t.Errorf("Run took %s after a 300ms timeout; the process group was not killed", elapsed)
	}

	cancelled, cancel := context.WithCancel(ctx)
	cancel()
	if _, err := f.g.Run(cancelled, f.checkout, "status"); !errors.Is(err, context.Canceled) {
		t.Errorf("cancelled context: %v", err)
	}
}

func TestLimitBuffer(t *testing.T) {
	b := &limitBuffer{limit: 4}
	for _, chunk := range []string{"ab", "cd", "ef"} {
		if n, err := b.Write([]byte(chunk)); n != 2 || err != nil {
			t.Fatalf("Write(%q) = %d, %v", chunk, n, err)
		}
	}
	if b.buf.String() != "abcd" || !b.truncated {
		t.Errorf("limitBuffer = %q truncated=%v", b.buf.String(), b.truncated)
	}
}
