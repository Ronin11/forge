package web

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/directives"
	"forge/internal/core/model"
	"forge/internal/core/store"
)

// A library edit whose runs regress against the pre-edit runs is git-reverted
// and its proposal marked reverted with the A/B numbers — the safety net that
// the restructure had silently disconnected (library edits never bump a
// routine generation, so the generation-keyed sweep could not see them).
func TestABRevertDirectiveEdit(t *testing.T) {
	f := newABFixture(t)

	// A git library with a base prompt and a worse edit on top.
	libDir := t.TempDir()
	if err := os.MkdirAll(filepath.Join(libDir, "directives"), 0o755); err != nil {
		t.Fatal(err)
	}
	git := func(args ...string) {
		t.Helper()
		out, err := exec.Command("git", append([]string{"-C", libDir, "-c", "user.name=t", "-c", "user.email=t@t"}, args...)...).CombinedOutput()
		if err != nil {
			t.Fatalf("git %v: %v %s", args, err, out)
		}
	}
	write := func(body string) {
		t.Helper()
		if err := os.WriteFile(filepath.Join(libDir, "directives", "triage-x.md"), []byte("---\nmode: run\nmodel: haiku\n---\n"+body+"\n"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	git("init", "-q", "-b", "main")
	write("old prompt {{objective}}")
	git("add", "-A")
	git("commit", "-q", "-m", "base")
	base := directives.Head(libDir)
	write("new, worse prompt {{objective}}")
	git("add", "-A")
	git("commit", "-q", "-m", "proposal: sharpen triage-x")
	edit := directives.Head(libDir)

	lib, err := directives.Load(libDir)
	if err != nil {
		t.Fatal(err)
	}
	current := lib
	f.srv.prompts = func() *directives.Library { return current }
	f.srv.promptsReload = func() error {
		next, lerr := directives.Load(libDir)
		if lerr != nil {
			return lerr
		}
		current = next
		return nil
	}

	// The applied proposal, exactly as apply.go records it.
	var pid string
	f.write(func(tx *store.Tx) error {
		p := &store.Proposal{Source: "retro:test", Kind: model.ProposalRoutine, Target: "routine:triage-x",
			After: json.RawMessage(`{"prompt":"new"}`), Rationale: "r", VerificationPlan: "A/B"}
		if err := tx.CreateProposal(actx(), p); err != nil {
			return err
		}
		pid = p.ID
		if _, err := tx.DecideProposal(actx(), p.ID, model.ProposalApproved, "human"); err != nil {
			return err
		}
		_, err := tx.MarkProposalApplied(actx(), p.ID, "directive:triage-x@"+edit)
		return err
	})

	// Five verified pre-edit runs, five failed post-edit runs, attributed to
	// the directive at each library commit.
	seed := func(commit string, verified bool, when time.Time) {
		a := f.attempt()
		state, pass := model.Failed, (*bool)(nil)
		if verified {
			v := true
			state, pass = model.Succeeded, &v
		}
		cost := 0.10
		f.write(func(tx *store.Tx) error {
			return tx.InsertFacts(actx(), &store.AttemptFacts{AttemptID: a.ID, TargetID: a.TargetID, Routine: "triage-x",
				Project: "default", Repository: "equitizr", Worker: testWorkerID, Executor: "claude-code",
				Model: "haiku", Mode: "run", Trigger: model.TriggerManual, Autonomy: model.AutonomyAuto,
				FinishedAt: when, State: state, VerificationPass: pass, CostUSD: &cost,
				Directive: "triage-x", LibraryCommit: commit})
		})
	}
	at := f.clock.Now().Add(-time.Hour)
	for i := 0; i < 5; i++ {
		seed(base, true, at.Add(time.Duration(i)*time.Second))
	}
	for i := 0; i < 5; i++ {
		seed(edit, false, at.Add(time.Duration(10+i)*time.Second))
	}

	f.srv.checkABReverts(actx(), config.ReflectionConfig{K: 5, Margin: 0.20})

	ps, err := f.st.ListProposals(actx(), model.ProposalReverted)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, p := range ps {
		if p.ID == pid {
			found = true
			if !strings.Contains(string(p.OutcomeMetrics), `"regressed_on":"rate"`) || !strings.Contains(string(p.OutcomeMetrics), edit) {
				t.Errorf("outcome = %s", p.OutcomeMetrics)
			}
		}
	}
	if !found {
		t.Fatalf("proposal not reverted; reverted = %+v", ps)
	}
	// The library content is back to the pre-edit prompt, hot-reloaded.
	raw, err := os.ReadFile(filepath.Join(libDir, "directives", "triage-x.md"))
	if err != nil || !strings.Contains(string(raw), "old prompt") {
		t.Fatalf("library not reverted: %v %s", err, raw)
	}
	if d := current.Directive("triage-x"); d == nil || !strings.Contains(d.Body, "old prompt") {
		t.Fatal("reload did not pick up the revert")
	}
}
