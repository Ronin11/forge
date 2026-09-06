package ui

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
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

// The Learning page composes the feed from works, proposals, experiments, and
// the library git log — a reflection run, an annotated library commit, and an
// experiment all render with the rollup header.
func TestLearningPage(t *testing.T) {
	ctx := context.Background()
	st, err := store.Open(ctx, filepath.Join(t.TempDir(), "forge.sqlite3"), store.Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	}()

	// A one-commit prompts library checkout.
	libDir := t.TempDir()
	if err := os.MkdirAll(filepath.Join(libDir, "directives"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(libDir, "directives", "plan-project.md"), []byte("---\nmode: plan\n---\nbody\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	git := func(args ...string) string {
		t.Helper()
		out, err := exec.Command("git", append([]string{"-C", libDir, "-c", "user.name=t", "-c", "user.email=t@t"}, args...)...).CombinedOutput()
		if err != nil {
			t.Fatalf("git %v: %v %s", args, err, out)
		}
		return strings.TrimSpace(string(out))
	}
	git("init", "-q", "-b", "master")
	git("add", "-A")
	git("commit", "-q", "-m", "plan-project: add efficiency guidance")
	sha := git("rev-parse", "HEAD")

	workerID := "0123456789abcdef0123456789abcdef"
	if err := st.Write(ctx, func(tx *store.Tx) error {
		if err := tx.EnsureProject(ctx, "default"); err != nil {
			return err
		}
		if err := tx.Register(ctx, protocol.RegisterRequest{WorkerID: workerID, Name: "laptop", Version: "test", MaxConcurrent: 2, Executors: []string{"claude-code"},
			Repositories: []protocol.Repository{{Name: "directives", Path: libDir, OriginIdentity: "local/directives"}}}); err != nil {
			return err
		}
		r := &store.Routine{Name: "reflect-library", Target: "directive:reflect-library", Repositories: []string{"directives"}, TimeoutSeconds: 300}
		if err := tx.CreateRoutine(ctx, r); err != nil {
			return err
		}
		w := &store.Work{RoutineID: r.ID, RoutineName: "reflect-library", Generation: 1, Title: "reflect on the library", Trigger: model.TriggerManual,
			Snapshot: []byte(`{}`), Priority: 50, BudgetClass: model.ClassBacklog, Autonomy: model.AutonomyAuto}
		targets, err := tx.CreateWork(ctx, w, []string{"directives"}, nil)
		if err != nil {
			return err
		}
		if _, err := tx.Claim(ctx, store.ClaimParams{TargetID: targets[0].ID, WorkerID: workerID, ClaimRequestID: "r1", LeaseToken: "l", MCPToken: "m",
			Executor: "claude-code", Model: "claude-haiku-4-5", ModelAlias: "haiku", Mode: "implement", Autonomy: model.AutonomyAuto}); err != nil {
			return err
		}
		// The commit above, landed by a proposal that the A/B net later reverted.
		p := &store.Proposal{Source: "retro:test", Kind: model.ProposalRoutine, Target: "routine:plan-project",
			After: json.RawMessage(`{"prompt":"x"}`), Rationale: "sharpen", VerificationPlan: "A/B"}
		if err := tx.CreateProposal(ctx, p); err != nil {
			return err
		}
		if _, err := tx.DecideProposal(ctx, p.ID, model.ProposalApproved, "human"); err != nil {
			return err
		}
		if _, err := tx.MarkProposalApplied(ctx, p.ID, "directive:plan-project@"+sha); err != nil {
			return err
		}
		pe := store.Experiment{Subject: "directive:reflect-library", Goal: "quote numbers exactly", TargetModel: "haiku", OptimizerModel: "haiku", Kind: store.ExperimentKindLive}
		return tx.InsertExperiment(ctx, &pe)
	}); err != nil {
		t.Fatal(err)
	}

	lib := &directives.Library{Dir: libDir}
	ui, err := NewUI(st, slog.New(slog.DiscardHandler), time.Now, func() *directives.Library { return lib })
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(ui.Handler())
	defer srv.Close()

	resp, err := http.Get(srv.URL + "/learning")
	if err != nil {
		t.Fatal(err)
	}
	body, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status %d: %s", resp.StatusCode, body)
	}
	for _, want := range []string{
		"Learning",
		"reflect on the library",           // the reflection work
		"plan-project: add efficiency",     // the library commit
		"via retro:test",                   // proposal annotation on the commit
		"quote numbers exactly",            // the experiment
		`data-f-directive="plan-project "`, // directive chip for filtering
		"learning spend",
	} {
		if !strings.Contains(string(body), want) {
			t.Errorf("learning page missing %q", want)
		}
	}
	if !strings.Contains(string(body), `href="/learning/commits/`+sha+`"`) {
		t.Error("commit row does not link to the diff view")
	}
	if !strings.Contains(string(body), `data-href="/experiments/`) {
		t.Error("experiment row is not clickable into its page")
	}

	// The experiment detail page renders (arms come from the row; this one
	// has none yet, so the header and goal are the assertion).
	expID := strings.Split(strings.Split(string(body), `data-href="/experiments/`)[1], `"`)[0]
	resp, err = http.Get(srv.URL + "/experiments/" + expID)
	if err != nil {
		t.Fatal(err)
	}
	body, _ = io.ReadAll(resp.Body)
	resp.Body.Close()
	for _, want := range []string{"Experiment", "quote numbers exactly", "directive:reflect-library"} {
		if !strings.Contains(string(body), want) {
			t.Errorf("experiment page missing %q", want)
		}
	}

	// The diff view: header, stat, classified add lines; a foreign sha gets
	// the unpushed-branch explanation; a malformed sha 404s.
	resp, err = http.Get(srv.URL + "/learning/commits/" + sha)
	if err != nil {
		t.Fatal(err)
	}
	body, _ = io.ReadAll(resp.Body)
	resp.Body.Close()
	// html/template renders "+" as "&#43;" in this context.
	for _, want := range []string{"plan-project: add efficiency", "d-add", "d-file", "directives/plan-project.md", "&#43;body"} {
		if !strings.Contains(string(body), want) {
			t.Errorf("commit page missing %q", want)
		}
	}
	resp, err = http.Get(srv.URL + "/learning/commits/" + strings.Repeat("0", 40))
	if err != nil {
		t.Fatal(err)
	}
	body, _ = io.ReadAll(resp.Body)
	resp.Body.Close()
	if !strings.Contains(string(body), "unpushed branch") {
		t.Errorf("missing-commit page: %s", body)
	}
	if resp, err = http.Get(srv.URL + "/learning/commits/nothex!"); err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusNotFound {
		t.Errorf("malformed sha = %d", resp.StatusCode)
	}
}

func TestLearningClassifiers(t *testing.T) {
	if got := learningWorkStatus("merged", ""); got != "landed" {
		t.Errorf("merged = %s", got)
	}
	if got := learningWorkStatus("unverified", "verify_verdict:fail"); got != "refuted" {
		t.Errorf("verify_verdict = %s", got)
	}
	if got := learningWorkStatus("unverified", "l0:changes_mismatch"); got != "unverified" {
		t.Errorf("l0 = %s", got)
	}
	if got := learningWorkStatus("claimed", ""); got != "running" {
		t.Errorf("claimed = %s", got)
	}
	if name, sha, ok := cutFragmentRef("directive:plan-project@abc123"); !ok || name != "plan-project" || sha != "abc123" {
		t.Errorf("cutFragmentRef = %s %s %v", name, sha, ok)
	}
	if _, _, ok := cutFragmentRef("generation:4"); ok {
		t.Error("generation ref parsed as fragment")
	}
	if n := fragmentName("directives/plan-project.md"); n != "plan-project" {
		t.Errorf("fragmentName = %s", n)
	}
	if n := fragmentName("README.md"); n != "" {
		t.Errorf("non-fragment path = %s", n)
	}
	if v := abVerdict(json.RawMessage(`{"regressed_on":"rate","prev_rate":0.8,"new_rate":0.2}`)); v != "regressed on rate: 80% → 20%" {
		t.Errorf("abVerdict = %s", v)
	}
	if v := liveVerdict(json.RawMessage(`{"winner":"v1","arms":[{"label":"control","runs":5,"verified_rate":0.4},{"label":"v1","runs":5,"verified_rate":0.8}]}`)); v != "winner v1 — control 40% (5) · v1 80% (5)" {
		t.Errorf("liveVerdict = %s", v)
	}
}
