package web

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/core/directives"
	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

func actx() context.Context { return context.Background() }

// applyFixture is a store plus a Server with a real home directory — the two
// things the apply engine touches.
type applyFixture struct {
	t    *testing.T
	st   *store.Store
	srv  *Server
	home string
}

func newApplyFixture(t *testing.T) *applyFixture {
	t.Helper()
	home := t.TempDir()
	clock := &fakeClock{now: time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)}
	st, err := store.Open(actx(), filepath.Join(t.TempDir(), "forge.sqlite3"), store.Options{Clock: clock.Now})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	})
	if err := st.Write(actx(), func(tx *store.Tx) error { return tx.EnsureProject(actx(), "default") }); err != nil {
		t.Fatal(err)
	}
	srv, err := NewServer(ServerOptions{Store: st, Clock: clock.Now, Version: "test", TransportOverride: transportUnix, Home: home})
	if err != nil {
		t.Fatal(err)
	}
	return &applyFixture{t: t, st: st, srv: srv, home: home}
}

func (f *applyFixture) write(fn func(tx *store.Tx) error) {
	f.t.Helper()
	if err := f.st.Write(actx(), fn); err != nil {
		f.t.Fatal(err)
	}
}

// approved creates one proposal and approves it, like the API would.
func (f *applyFixture) approved(kind model.ProposalKind, target, after string) *store.Proposal {
	f.t.Helper()
	p := &store.Proposal{Source: "manual", Kind: kind, Target: target, After: json.RawMessage(after),
		Rationale: "because it regressed", VerificationPlan: "watch the next runs"}
	f.write(func(tx *store.Tx) error {
		if err := tx.CreateProposal(actx(), p); err != nil {
			return err
		}
		_, err := tx.DecideProposal(actx(), p.ID, model.ProposalApproved, "nate")
		return err
	})
	return p
}

// apply runs applyProposal in one transaction and marks the proposal applied,
// exactly as the approve handler does.
func (f *applyFixture) apply(p *store.Proposal) (string, error) {
	f.t.Helper()
	var ref string
	err := f.st.Write(actx(), func(tx *store.Tx) error {
		var aerr error
		ref, aerr = f.srv.applyProposal(actx(), tx, p)
		if aerr != nil {
			return aerr
		}
		_, aerr = tx.MarkProposalApplied(actx(), p.ID, ref)
		return aerr
	})
	return ref, err
}

func (f *applyFixture) createRoutine(name string) {
	f.t.Helper()
	f.write(func(tx *store.Tx) error {
		return tx.CreateRoutine(actx(), &store.Routine{Name: name, Mode: "run", Prompt: "old prompt", Model: "haiku", TimeoutSeconds: 300})
	})
}

func (f *applyFixture) routine(name string) *store.Routine {
	f.t.Helper()
	r, err := f.st.GetRoutine(actx(), name)
	if err != nil {
		f.t.Fatal(err)
	}
	return r
}

func (f *applyFixture) proposal(id string) *store.Proposal {
	f.t.Helper()
	p, err := f.st.GetProposal(actx(), id)
	if err != nil {
		f.t.Fatal(err)
	}
	return p
}

func TestApplyRoutineProposal(t *testing.T) {
	f := newApplyFixture(t)
	f.createRoutine("inventory")
	p := f.approved(model.ProposalRoutine, "routine:inventory", `{"prompt":"new prompt","max_turns":7}`)
	ref, err := f.apply(p)
	if err != nil {
		t.Fatal(err)
	}
	if ref != "generation:2" {
		t.Errorf("ref = %q, want generation:2", ref)
	}
	r := f.routine("inventory")
	if r.Prompt != "new prompt" || r.MaxTurns != 7 || r.Generation != 2 || r.Model != "haiku" {
		t.Errorf("routine after apply = %+v", r)
	}
	got := f.proposal(p.ID)
	if got.Status != model.ProposalApplied || got.AppliedRef != ref {
		t.Errorf("proposal after apply = %+v", got)
	}
	// The bump was recorded as a generation with the pre-apply snapshot intact.
	f.write(func(tx *store.Tx) error {
		snap, err := tx.GenerationSnapshot(actx(), r.ID, 1)
		if err != nil {
			return err
		}
		var old store.Routine
		if err := json.Unmarshal(snap, &old); err != nil {
			return err
		}
		if old.Prompt != "old prompt" {
			t.Errorf("generation 1 snapshot prompt = %q", old.Prompt)
		}
		return nil
	})
}

func TestApplyRoutineProposalUnknownField(t *testing.T) {
	f := newApplyFixture(t)
	f.createRoutine("inventory")
	// schedule is a process field; a routine proposal must not carry it.
	p := f.approved(model.ProposalRoutine, "routine:inventory", `{"schedule":"0 4 * * *"}`)
	if _, err := f.apply(p); err == nil {
		t.Fatal("apply with an unknown after-field succeeded, want error")
	}
	if got := f.proposal(p.ID); got.Status != model.ProposalApproved {
		t.Errorf("proposal after failed apply = %s, want approved (rolled back)", got.Status)
	}
	if r := f.routine("inventory"); r.Generation != 1 {
		t.Errorf("generation after failed apply = %d, want 1", r.Generation)
	}
}

func TestApplyProcessProposal(t *testing.T) {
	f := newApplyFixture(t)
	f.createRoutine("nightly")
	p := f.approved(model.ProposalProcess, "routine:nightly",
		`{"schedule":"0 4 * * *","schedule_enabled":true,"budget_class":"backlog","priority":10}`)
	ref, err := f.apply(p)
	if err != nil {
		t.Fatal(err)
	}
	if ref != "generation:2" {
		t.Errorf("ref = %q, want generation:2", ref)
	}
	r := f.routine("nightly")
	if r.Schedule != "0 4 * * *" || !r.ScheduleEnabled || r.BudgetClass != model.ClassBacklog || r.Priority != 10 || r.Generation != 2 {
		t.Errorf("routine after process apply = %+v", r)
	}
	if r.Prompt != "old prompt" {
		t.Errorf("prompt changed by a process proposal: %q", r.Prompt)
	}
}

func TestApplyModePromptProposal(t *testing.T) {
	f := newApplyFixture(t)
	modes := filepath.Join(f.home, "modes")
	if err := os.MkdirAll(modes, 0o700); err != nil {
		t.Fatal(err)
	}
	live := filepath.Join(modes, "run.md")
	if err := os.WriteFile(live, []byte("OLD PREAMBLE"), 0o600); err != nil {
		t.Fatal(err)
	}
	p := f.approved(model.ProposalModePrompt, "mode:run", `{"content":"NEW PREAMBLE"}`)
	ref, err := f.apply(p)
	if err != nil {
		t.Fatal(err)
	}
	if ref != live {
		t.Errorf("ref = %q, want %q", ref, live)
	}
	got, err := os.ReadFile(live)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != "NEW PREAMBLE" {
		t.Errorf("live preamble = %q", got)
	}
	backup, err := os.ReadFile(filepath.Join(modes, "run.md.prev-"+model.ShortID(p.ID)))
	if err != nil {
		t.Fatal(err)
	}
	if string(backup) != "OLD PREAMBLE" {
		t.Errorf("backup = %q", backup)
	}
}

func TestApplyDocProposal(t *testing.T) {
	f := newApplyFixture(t)
	p := f.approved(model.ProposalDoc, "kb:some-note", `{"content":"agent-written"}`)
	ref, err := f.apply(p)
	if err != nil {
		t.Fatal(err)
	}
	if ref != "recorded:kb:some-note" {
		t.Errorf("ref = %q", ref)
	}
}

// toolAfterJSON builds a tool proposal's after with the given test script body.
func toolAfterJSON(t *testing.T, name, testBody string) string {
	t.Helper()
	after := map[string]any{
		"manifest": map[string]any{
			"name":         name,
			"description":  "echoes stdin",
			"input_schema": map[string]any{"type": "object", "additionalProperties": true},
			"command":      []string{"./run.sh"},
			"test_command": []string{"./test.sh"},
		},
		"files": map[string]string{
			"run.sh":  "#!/bin/sh\ncat\n",
			"test.sh": testBody,
		},
	}
	b, err := json.Marshal(after)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

func TestApplyToolProposal(t *testing.T) {
	f := newApplyFixture(t)
	p := f.approved(model.ProposalTool, "echoer", toolAfterJSON(t, "echoer", "#!/bin/sh\nexit 0\n"))
	ref, err := f.apply(p)
	if err != nil {
		t.Fatal(err)
	}
	dir := filepath.Join(f.home, "tools", "echoer")
	if ref != dir {
		t.Errorf("ref = %q, want %q", ref, dir)
	}
	if _, err := os.Stat(filepath.Join(dir, "manifest.toml")); err != nil {
		t.Errorf("manifest.toml: %v", err)
	}
	info, err := os.Stat(filepath.Join(dir, "run.sh"))
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm()&0o111 == 0 {
		t.Errorf("run.sh mode = %v, want executable", info.Mode())
	}
}

func TestApplyToolProposalFailingTest(t *testing.T) {
	f := newApplyFixture(t)
	p := f.approved(model.ProposalTool, "broken", toolAfterJSON(t, "broken", "#!/bin/sh\nexit 1\n"))
	if _, err := f.apply(p); err == nil {
		t.Fatal("apply with a failing test_command succeeded, want error")
	}
	if _, err := os.Stat(filepath.Join(f.home, "tools", "broken")); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("tool dir after failed apply: stat = %v, want not exist", err)
	}
	if got := f.proposal(p.ID); got.Status != model.ProposalApproved {
		t.Errorf("proposal after failed apply = %s, want approved (rolled back)", got.Status)
	}
}

// gitTest runs one git command in a test repository with identity by env.
func gitTest(t *testing.T, dir string, args ...string) string {
	t.Helper()
	cmd := exec.Command("git", append([]string{"-C", dir}, args...)...)
	cmd.Env = append(os.Environ(),
		"GIT_AUTHOR_NAME=test", "GIT_AUTHOR_EMAIL=test@local",
		"GIT_COMMITTER_NAME=test", "GIT_COMMITTER_EMAIL=test@local",
		"GIT_CONFIG_GLOBAL=/dev/null", "GIT_CONFIG_SYSTEM=/dev/null")
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git %v: %v: %s", args, err, out)
	}
	return strings.TrimSpace(string(out))
}

func TestApplyCodeProposal(t *testing.T) {
	f := newApplyFixture(t)
	repo := t.TempDir()
	gitTest(t, repo, "init", "-q")
	if err := os.WriteFile(filepath.Join(repo, "README.md"), []byte("forge\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	gitTest(t, repo, "add", "-A")
	gitTest(t, repo, "commit", "-q", "-m", "init")
	headBefore := gitTest(t, repo, "rev-parse", "HEAD")
	branchBefore := gitTest(t, repo, "rev-parse", "--abbrev-ref", "HEAD")
	f.write(func(tx *store.Tx) error {
		return tx.Register(actx(), protocol.RegisterRequest{WorkerID: testWorkerID, Name: "laptop", Version: "test", MaxConcurrent: 1,
			Executors:    []string{"claude-code"},
			Repositories: []protocol.Repository{{Name: "forge", Path: repo, OriginIdentity: "github.com/x/forge"}}})
	})

	const diff = "--- a/queue.go\n+++ b/queue.go\n"
	p := f.approved(model.ProposalCode, "internal/controlplane/queue.go", `{"diff":"--- a/queue.go\n+++ b/queue.go\n"}`)
	ref, err := f.apply(p)
	if err != nil {
		t.Fatal(err)
	}
	branch := "forge/proposal-" + model.ShortID(p.ID)
	if ref != branch {
		t.Errorf("ref = %q, want %q", ref, branch)
	}
	gitTest(t, repo, "rev-parse", "--verify", "refs/heads/"+branch)
	if doc := gitTest(t, repo, "show", branch+":PROPOSAL.md"); !strings.Contains(doc, "because it regressed") || !strings.Contains(doc, "internal/controlplane/queue.go") {
		t.Errorf("PROPOSAL.md = %q", doc)
	}
	if got := gitTest(t, repo, "show", branch+":proposal.diff"); got != strings.TrimSpace(diff) {
		t.Errorf("proposal.diff = %q, want %q", got, diff)
	}

	// The registered checkout was never touched: same HEAD, same branch, clean
	// tree, one worktree, no remotes to have pushed to.
	if got := gitTest(t, repo, "rev-parse", "HEAD"); got != headBefore {
		t.Errorf("HEAD moved: %s → %s", headBefore, got)
	}
	if got := gitTest(t, repo, "rev-parse", "--abbrev-ref", "HEAD"); got != branchBefore {
		t.Errorf("checked-out branch changed: %s → %s", branchBefore, got)
	}
	if got := gitTest(t, repo, "status", "--porcelain"); got != "" {
		t.Errorf("working tree dirty after apply:\n%s", got)
	}
	if wt := gitTest(t, repo, "worktree", "list"); strings.Count(wt, "\n") != 0 {
		t.Errorf("scratch worktree left behind:\n%s", wt)
	}
	if remotes := gitTest(t, repo, "remote"); remotes != "" {
		t.Errorf("unexpected remotes: %q", remotes)
	}
	entries, err := os.ReadDir(filepath.Join(f.home, "scratch"))
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 0 {
		t.Errorf("scratch dir not empty: %v", entries)
	}

	// The same proposal applied again hits the existing branch.
	err = f.st.Write(actx(), func(tx *store.Tx) error {
		_, aerr := f.srv.applyProposal(actx(), tx, p)
		return aerr
	})
	if err == nil || !strings.Contains(err.Error(), "already exists") {
		t.Errorf("second apply = %v, want branch-exists error", err)
	}
}

func TestApplyCodeProposalNoForgeRepo(t *testing.T) {
	f := newApplyFixture(t)
	p := f.approved(model.ProposalCode, "internal/x", `{"diff":"d"}`)
	if _, err := f.apply(p); err == nil || !strings.Contains(err.Error(), `"forge"`) {
		t.Errorf("apply without a forge repository = %v, want missing-repo error", err)
	}
}

// Bare targets (no kind prefix) are accepted: retro agents file both forms.
func TestTargetNameBare(t *testing.T) {
	for _, in := range []string{"inventory", "routine:inventory"} {
		name, err := targetName(in, "routine:")
		if err != nil || name != "inventory" {
			t.Errorf("targetName(%q) = %q, %v", in, name, err)
		}
	}
	if _, err := targetName("routine:bad name!", "routine:"); err == nil {
		t.Error("invalid name accepted")
	}
}

// A routine proposal against a directive-target routine writes its content
// updates to the library file (validated, committed) while operational
// updates still bump the row; a breaking content edit reverts the file and
// rolls the approval back.
func TestApplyDirectiveRoutineProposal(t *testing.T) {
	f := newApplyFixture(t)
	dir := t.TempDir()
	for path, content := range map[string]string{
		"directives/triage.md": "---\nmode: run\nmodel: haiku\n---\nOld task: {{objective}}\n",
	} {
		full := filepath.Join(dir, path)
		if err := os.MkdirAll(filepath.Dir(full), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(full, []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	lib, err := directives.Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	current := lib
	f.srv.prompts = func() *directives.Library { return current }
	f.srv.promptsReload = func() error {
		next, err := directives.Load(dir)
		if err != nil {
			return err
		}
		current = next
		return nil
	}
	f.write(func(tx *store.Tx) error {
		return tx.CreateRoutine(actx(), &store.Routine{Name: "triage-trigger", Target: "directive:triage", TimeoutSeconds: 300})
	})

	p := f.approved(model.ProposalRoutine, "routine:triage-trigger", `{"prompt":"New task: {{objective}}","model":"sonnet","max_turns":9}`)
	ref, err := f.apply(p)
	if err != nil {
		t.Fatal(err)
	}
	if ref != "directive:triage" {
		t.Errorf("ref = %q", ref)
	}
	raw, err := os.ReadFile(filepath.Join(dir, "directives", "triage.md"))
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"New task: {{objective}}", "model: sonnet", "mode: run"} {
		if !strings.Contains(string(raw), want) {
			t.Errorf("file missing %q:\n%s", want, raw)
		}
	}
	if strings.Contains(string(raw), "Old task") {
		t.Errorf("old body remains:\n%s", raw)
	}
	// Operational field still hit the row; content never did.
	r := f.routine("triage-trigger")
	if r.MaxTurns != 9 || r.Prompt != "" || r.Model != "" || r.Generation != 2 {
		t.Errorf("row after apply = %+v", r)
	}
	// The hot-reload swapped the library.
	if d := current.Directive("triage"); d == nil || d.Model != "sonnet" {
		t.Errorf("library after apply = %+v", d)
	}

	// A breaking edit reverts the file and the approval.
	bad := f.approved(model.ProposalRoutine, "routine:triage-trigger", `{"prompt":"{{> ghost}}"}`)
	if _, err := f.apply(bad); err == nil {
		t.Fatal("breaking edit applied")
	}
	raw, err = os.ReadFile(filepath.Join(dir, "directives", "triage.md"))
	if err != nil || !strings.Contains(string(raw), "New task") {
		t.Errorf("file not reverted: %v\n%s", err, raw)
	}
	if got := f.proposal(bad.ID); got.Status != model.ProposalApproved {
		t.Errorf("proposal after failed apply = %s", got.Status)
	}
}

func TestRewriteDirective(t *testing.T) {
	raw := []byte("---\nmode: run\npersona: triager\nmodel: haiku\n---\nOld body.\n")
	sp := func(s string) *string { return &s }
	out, err := directives.RewriteDirective(raw, directives.DirectiveUpdates{Body: sp("New body."), Model: sp("opus"), Effort: sp("high")})
	if err != nil {
		t.Fatal(err)
	}
	want := "---\nmode: run\npersona: triager\nmodel: opus\neffort: high\n---\nNew body.\n"
	if string(out) != want {
		t.Errorf("rewrite = %q, want %q", out, want)
	}
	// Clearing a key removes it; nil keeps.
	out, err = directives.RewriteDirective(raw, directives.DirectiveUpdates{Model: sp("")})
	if err != nil || strings.Contains(string(out), "model:") || !strings.Contains(string(out), "Old body.") {
		t.Errorf("clear model = %q, %v", out, err)
	}
	if _, err := directives.RewriteDirective([]byte("no frontmatter"), directives.DirectiveUpdates{}); err == nil {
		t.Error("frontmatter-less accepted")
	}
}
