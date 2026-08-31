package integrator

import (
	"context"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/model"
	"forge/internal/protocol"
	"forge/internal/store"
)

const workerID = "0123456789abcdef0123456789abcdef"

// gitRun runs one git command for test setup; failures kill the test.
func gitRun(t *testing.T, dir string, args ...string) string {
	t.Helper()
	cmd := exec.Command("git", args...)
	cmd.Dir = dir
	cmd.Env = append(os.Environ(),
		"GIT_AUTHOR_NAME=t", "GIT_AUTHOR_EMAIL=t@t", "GIT_COMMITTER_NAME=t", "GIT_COMMITTER_EMAIL=t@t",
		"GIT_CONFIG_COUNT=1", "GIT_CONFIG_KEY_0=commit.gpgsign", "GIT_CONFIG_VALUE_0=false")
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git %v in %s: %v\n%s", args, dir, err, out)
	}
	return strings.TrimSpace(string(out))
}

func writeFile(t *testing.T, dir, name, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(filepath.Join(dir, name)), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, name), []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
}

// repos builds a bare origin and a checkout whose master carries forge.toml
// (integration_branch, an always-green check) and a base file.
func repos(t *testing.T, forgeToml string) (checkout, bare string) {
	t.Helper()
	root := t.TempDir()
	bare = filepath.Join(root, "origin.git")
	gitRun(t, root, "init", "--bare", "-b", "master", bare)
	checkout = filepath.Join(root, "checkout")
	gitRun(t, root, "clone", bare, checkout)
	gitRun(t, checkout, "checkout", "-b", "master")
	writeFile(t, checkout, "forge.toml", forgeToml)
	writeFile(t, checkout, "main.go", "package main\n\nfunc a() int { return 1 }\n\nfunc z() int { return 26 }\n")
	gitRun(t, checkout, "add", ".")
	gitRun(t, checkout, "commit", "-qm", "base")
	gitRun(t, checkout, "push", "-q", "origin", "master")
	return checkout, bare
}

const greenToml = "integration_branch = \"master\"\ntask_branches = \"forge/*\"\n\n[checks]\nok = [\"true\"]\n"

// taskBranch commits files on a new branch cut at master, in a temp worktree,
// the way a worker attempt would; the branch stays in the checkout's refs.
func taskBranch(t *testing.T, checkout, branch string, files map[string]string) (head string) {
	t.Helper()
	wt := filepath.Join(t.TempDir(), strings.ReplaceAll(branch, "/", "-"))
	gitRun(t, checkout, "worktree", "add", "-b", branch, wt, "master")
	for name, content := range files {
		writeFile(t, wt, name, content)
	}
	gitRun(t, wt, "add", ".")
	gitRun(t, wt, "commit", "-qm", "task "+branch)
	head = gitRun(t, wt, "rev-parse", "HEAD")
	gitRun(t, checkout, "worktree", "remove", "--force", wt)
	return head
}

// harness owns a store with one registered repository and helpers to walk an
// integrating Target into the merge queue through the public store API.
type harness struct {
	t        *testing.T
	st       *store.Store
	checkout string
	bare     string
	home     string
	integ    *Integrator
	n        int
}

func newHarness(t *testing.T, forgeToml string) *harness {
	t.Helper()
	checkout, bare := repos(t, forgeToml)
	st, err := store.Open(context.Background(), filepath.Join(t.TempDir(), "forge.sqlite3"), store.Options{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	})
	err = st.Write(context.Background(), func(tx *store.Tx) error {
		if err := tx.EnsureProject(context.Background(), "default"); err != nil {
			return err
		}
		return tx.Register(context.Background(), protocol.RegisterRequest{WorkerID: workerID, Name: "laptop", Version: "test", MaxConcurrent: 1, Executors: []string{"fake-claude"},
			Repositories: []protocol.Repository{{Name: "scratchrepo", Path: checkout, OriginIdentity: "file://" + bare}}})
	})
	if err != nil {
		t.Fatal(err)
	}
	home := t.TempDir()
	h := &harness{t: t, st: st, checkout: checkout, bare: bare, home: home}
	h.integ = New(st, slog.New(slog.DiscardHandler), time.Now, Config{Home: home, MaxRebaseAttempts: 2, Interval: time.Hour})
	return h
}

// queue creates an integrating Work, claims it, records the branch, completes
// it as verified-succeeded, and moves it into the merge queue. It returns the
// Target and attempt ids.
func (h *harness) queue(branch, head string) (targetID, attemptID string) {
	h.t.Helper()
	h.n++
	req := "req-" + branch + "-" + strings.Repeat("x", h.n)
	lease := "lease-" + req
	ctx := context.Background()
	w := &store.Work{RoutineName: "coder", Generation: 1, Title: branch, Trigger: model.TriggerManual, Snapshot: []byte(`{}`), Priority: 100, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto, Integrate: true}
	err := h.st.Write(ctx, func(tx *store.Tx) error {
		targets, err := tx.CreateWork(ctx, w, []string{"scratchrepo"}, nil)
		if err != nil {
			return err
		}
		targetID = targets[0].ID
		a, err := tx.Claim(ctx, store.ClaimParams{TargetID: targetID, WorkerID: workerID, ClaimRequestID: req, LeaseToken: lease, MCPToken: "m" + req, Executor: "fake-claude", Model: "m", ModelAlias: "haiku", Mode: "run", Autonomy: model.AutonomyAuto})
		if err != nil {
			return err
		}
		attemptID = a.ID
		for _, st := range []model.State{model.Preparing, model.Running} {
			if _, err := tx.RecordHeartbeat(ctx, a.ID, protocol.HeartbeatRequest{LeaseToken: lease, State: st, Branch: branch, BaseBranch: "master"}); err != nil {
				return err
			}
		}
		if _, err := tx.Complete(ctx, a.ID, protocol.CompleteRequest{LeaseToken: lease, State: model.Succeeded, Git: protocol.GitOutcome{Head: head, Commits: 1}, Verification: protocol.Verification{Level: 1, Passed: true}, FinishedAt: time.Now().UTC()}, 1); err != nil {
			return err
		}
		_, err = tx.Transition(ctx, targetID, model.QueuedForMerge, store.TransitionOptions{Actor: "daemon"})
		return err
	})
	if err != nil {
		h.t.Fatal(err)
	}
	return targetID, attemptID
}

func (h *harness) target(id string) *store.Target {
	h.t.Helper()
	tg, err := h.st.GetTarget(context.Background(), id)
	if err != nil {
		h.t.Fatal(err)
	}
	return tg
}

func (h *harness) remoteHead(t *testing.T) string {
	return gitRun(t, h.bare, "rev-parse", "master")
}

// A clean rebase merges: the remote integration branch advances fast-forward
// only, the push is journaled with before/after SHAs, and the Target is
// merged.
func TestCleanMergeAdvancesRemote(t *testing.T) {
	h := newHarness(t, greenToml)
	before := h.remoteHead(t)
	head := taskBranch(t, h.checkout, "forge/one", map[string]string{"one.txt": "one\n"})
	targetID, attemptID := h.queue("forge/one", head)
	h.integ.Tick(context.Background())

	if tg := h.target(targetID); tg.State != model.Merged {
		t.Fatalf("target = %s, want merged", tg.State)
	}
	after := h.remoteHead(t)
	if after == before {
		t.Fatal("remote integration branch did not advance")
	}
	// Fast-forward only: the old head is an ancestor of the new one.
	gitRun(t, h.bare, "merge-base", "--is-ancestor", before, after)
	m, err := h.st.MergeForTarget(context.Background(), targetID)
	if err != nil || m == nil || m.Outcome != "merged" || m.BeforeSHA != before || m.AfterSHA != after {
		t.Fatalf("merge row = %+v, %v", m, err)
	}
	pushed := false
	for _, e := range must(h.st.JournalForEntity(context.Background(), store.EntityMerge, m.ID)) {
		if e.Kind == "merge.pushed" && strings.Contains(string(e.Payload), before) && strings.Contains(string(e.Payload), after) {
			pushed = true
		}
	}
	if !pushed {
		t.Fatal("merge.pushed journal row with SHAs missing")
	}
	if _, err := os.Stat(filepath.Join(h.home, "integrator", "scratchrepo")); !os.IsNotExist(err) {
		// The per-target scratch dir is removed; the repo dir may remain empty.
		entries, rerr := os.ReadDir(filepath.Join(h.home, "integrator", "scratchrepo"))
		if rerr != nil {
			t.Fatal(rerr)
		}
		if len(entries) != 0 {
			t.Errorf("scratch not cleaned: %v", entries)
		}
	}
	_ = attemptID
}

// Three tasks merge in order, each rebased onto the previous result.
func TestSerialMergesStack(t *testing.T) {
	h := newHarness(t, greenToml)
	var ids []string
	for i, name := range []string{"forge/a", "forge/b", "forge/c"} {
		head := taskBranch(t, h.checkout, name, map[string]string{"f" + string(rune('a'+i)) + ".txt": name + "\n"})
		id, _ := h.queue(name, head)
		ids = append(ids, id)
	}
	// One target per tick per repository: three ticks.
	for range 3 {
		h.integ.Tick(context.Background())
	}
	for _, id := range ids {
		if tg := h.target(id); tg.State != model.Merged {
			t.Fatalf("target %s = %s, want merged", id, tg.State)
		}
	}
	out := gitRun(t, h.bare, "rev-list", "--count", "master")
	if out != "4" { // base + three task commits
		t.Fatalf("remote commits = %s, want 4", out)
	}
}

// A real conflict (two tasks editing the same line, no mergiraf resolution
// possible) lands in conflict with the scratch clone retained.
func TestConflictRetains(t *testing.T) {
	h := newHarness(t, greenToml)
	headA := taskBranch(t, h.checkout, "forge/left", map[string]string{"shared.txt": "left version\n"})
	headB := taskBranch(t, h.checkout, "forge/right", map[string]string{"shared.txt": "right version\n"})
	idA, _ := h.queue("forge/left", headA)
	idB, _ := h.queue("forge/right", headB)
	h.integ.Tick(context.Background())
	h.integ.Tick(context.Background())
	if tg := h.target(idA); tg.State != model.Merged {
		t.Fatalf("first target = %s, want merged", tg.State)
	}
	tg := h.target(idB)
	if tg.State != model.Conflict || !tg.Retained {
		t.Fatalf("second target = %s retained=%v, want conflict retained", tg.State, tg.Retained)
	}
	m, err := h.st.MergeForTarget(context.Background(), idB)
	if err != nil || m == nil || m.Outcome != "conflict" {
		t.Fatalf("merge row = %+v, %v", m, err)
	}
	scratch := filepath.Join(h.home, "integrator", "scratchrepo", model.ShortID(idB))
	if _, err := os.Stat(scratch); err != nil {
		t.Fatalf("scratch clone must be retained on conflict: %v", err)
	}
	// The conflicted state is inspectable: markers or an in-progress rebase.
	if _, err := os.Stat(filepath.Join(scratch, ".git", "rebase-merge")); err != nil {
		t.Errorf("rebase state missing in retained scratch: %v", err)
	}
}

// mergiraf resolves an adjacent-addition conflict in a Go file with no agent
// involved: two tasks adding different functions at the same spot both merge.
func TestMergirafAdjacentAddition(t *testing.T) {
	if _, err := exec.LookPath("mergiraf"); err != nil {
		t.Skip("mergiraf not on PATH")
	}
	h := newHarness(t, greenToml)
	headA := taskBranch(t, h.checkout, "forge/funcb", map[string]string{"main.go": "package main\n\nfunc a() int { return 1 }\n\nfunc b() int { return 2 }\n\nfunc z() int { return 26 }\n"})
	headB := taskBranch(t, h.checkout, "forge/funcc", map[string]string{"main.go": "package main\n\nfunc a() int { return 1 }\n\nfunc c() int { return 3 }\n\nfunc z() int { return 26 }\n"})
	idA, _ := h.queue("forge/funcb", headA)
	idB, _ := h.queue("forge/funcc", headB)
	h.integ.Tick(context.Background())
	h.integ.Tick(context.Background())
	if tg := h.target(idA); tg.State != model.Merged {
		t.Fatalf("first target = %s", tg.State)
	}
	if tg := h.target(idB); tg.State != model.Merged {
		t.Fatalf("second target = %s, want merged via mergiraf", tg.State)
	}
	// The merged file carries both functions.
	show := gitRun(t, h.bare, "show", "master:main.go")
	if !strings.Contains(show, "func b()") || !strings.Contains(show, "func c()") {
		t.Fatalf("merged main.go lost a function:\n%s", show)
	}
}

// Declared checks failing on the rebased result: unverified with the check
// named, and nothing pushed.
func TestChecksFailOnRebasedResult(t *testing.T) {
	h := newHarness(t, "integration_branch = \"master\"\n\n[checks]\nnope = [\"false\"]\n")
	before := h.remoteHead(t)
	head := taskBranch(t, h.checkout, "forge/bad", map[string]string{"bad.txt": "x\n"})
	id, _ := h.queue("forge/bad", head)
	h.integ.Tick(context.Background())
	tg := h.target(id)
	if tg.State != model.Unverified || !strings.HasPrefix(tg.UnverifiedReason, "check_failed:") {
		t.Fatalf("target = %s (%s)", tg.State, tg.UnverifiedReason)
	}
	if h.remoteHead(t) != before {
		t.Fatal("remote must not advance when checks fail")
	}
}

// No integration_branch declared: the merge is refused into conflict and
// journaled; nothing is pushed (constitution 10).
func TestNoIntegrationBranchRefused(t *testing.T) {
	h := newHarness(t, "[checks]\nok = [\"true\"]\n")
	before := h.remoteHead(t)
	head := taskBranch(t, h.checkout, "forge/refuse", map[string]string{"r.txt": "x\n"})
	id, _ := h.queue("forge/refuse", head)
	h.integ.Tick(context.Background())
	if tg := h.target(id); tg.State != model.Conflict {
		t.Fatalf("target = %s, want conflict", tg.State)
	}
	if h.remoteHead(t) != before {
		t.Fatal("remote must not move")
	}
	refused := false
	for _, e := range must(h.st.JournalForEntity(context.Background(), store.EntityTarget, id)) {
		if e.Kind == "merge.refused" {
			refused = true
		}
	}
	if !refused {
		t.Fatal("merge.refused journal row missing")
	}
}

// A crash that left a merging Target with no lease is requeued at tick start.
func TestRecoverStaleMerging(t *testing.T) {
	h := newHarness(t, greenToml)
	head := taskBranch(t, h.checkout, "forge/stale", map[string]string{"s.txt": "x\n"})
	id, _ := h.queue("forge/stale", head)
	err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		_, terr := tx.Transition(context.Background(), id, model.Merging, store.TransitionOptions{Actor: "integrator"})
		return terr
	})
	if err != nil {
		t.Fatal(err)
	}
	h.integ.Tick(context.Background())
	if tg := h.target(id); tg.State != model.Merged {
		t.Fatalf("stale merging target = %s, want recovered and merged", tg.State)
	}
}

// must returns v or panics on err; a panic in a test is a failure with a
// stack trace, which is what an unexpected store error deserves.
func must[T any](v T, err error) T {
	if err != nil {
		panic(err)
	}
	return v
}
