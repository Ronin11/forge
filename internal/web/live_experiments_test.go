package web

import (
	"context"
	"encoding/json"
	"net/http"
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

// openLiveExperiment inserts a live-kind experiment for a directive already
// in the harness library and opens it with control + one variant arm.
func openLiveExperiment(t *testing.T, st *store.Store, lib *directives.Library, subject, name, variantBody string) string {
	t.Helper()
	return openLiveExperimentBy(t, st, lib, subject, name, variantBody, time.Now().Add(24*time.Hour))
}

func openLiveExperimentBy(t *testing.T, st *store.Store, lib *directives.Library, subject, name, variantBody string, decideBy time.Time) string {
	t.Helper()
	frag := lib.Fragment(name)
	if frag == nil {
		t.Fatalf("fragment %s not in library", name)
	}
	raw, err := os.ReadFile(frag.Path)
	if err != nil {
		t.Fatal(err)
	}
	arms, _ := json.Marshal([]store.ExperimentArm{
		{Label: "control", Content: string(raw), Hash: frag.Hash},
		{Label: "v1", Title: "variant", Content: variantBody, Hash: hashContent(variantBody)},
	})
	pe := store.Experiment{Subject: subject, Goal: "test", TargetModel: "haiku", OptimizerModel: "haiku", Kind: store.ExperimentKindLive}
	if err := st.Write(context.Background(), func(tx *store.Tx) error {
		if err := tx.InsertExperiment(context.Background(), &pe); err != nil {
			return err
		}
		return tx.SetExperimentLive(context.Background(), pe.ID, string(raw), arms, 2, decideBy)
	}); err != nil {
		t.Fatal(err)
	}
	return pe.ID
}

// Round-robin assignment: root works cycle arms, compositions stamp the
// experiment and label, control composes byte-identically to an unassigned
// run, and continuations (CausedBy set) never enroll.
func TestLiveAssignmentRoundRobin(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutineWith("expdir", "base prompt {{objective}}")
	variant := "---\nmode: run\nmodel: haiku\n---\nVARIANT prompt {{objective}}\n"
	expID := openLiveExperiment(t, h.st, h.srv.promptLibrary(), "directive:expdir", "expdir", variant)
	h.srv.refreshLiveExperiments(context.Background())

	var labels []string
	var prompts []string
	for i := 0; i < 4; i++ {
		created := h.run("expdir")
		var comp struct {
			Experiment string `json:"experiment"`
			Variant    string `json:"variant"`
		}
		if err := json.Unmarshal(created.Work.Composition, &comp); err != nil {
			t.Fatalf("composition: %v (%s)", err, created.Work.Composition)
		}
		if comp.Experiment != expID {
			t.Fatalf("work %d experiment = %q, want %q", i, comp.Experiment, expID)
		}
		labels = append(labels, comp.Variant)
		var snap store.Routine
		if err := json.Unmarshal(created.Work.Snapshot, &snap); err != nil {
			t.Fatal(err)
		}
		prompts = append(prompts, snap.Prompt)
	}
	if got := strings.Join(labels, ","); got != "control,v1,control,v1" {
		t.Fatalf("labels = %s", got)
	}
	if !strings.Contains(prompts[1], "VARIANT prompt") || strings.Contains(prompts[0], "VARIANT") {
		t.Fatalf("arm bodies wrong:\ncontrol=%q\nv1=%q", prompts[0], prompts[1])
	}

	// A continuation-shaped request (CausedBy set) does not enroll.
	parent := h.run("expdir") // consumes an arm slot; fine
	var out workCreated
	err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		var werr error
		out, werr = h.srv.createWorkTx(context.Background(), tx, workRequest{
			Routine: "expdir", Objective: "child",
			Class: model.ClassBacklog, CausedBy: parent.Work.ID, cause: model.CauseTool, Autonomy: model.AutonomyAuto,
		})
		return werr
	})
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(out.Work.Composition), `"experiment"`) {
		t.Fatalf("caused work enrolled: %s", out.Work.Composition)
	}
}

// The decision pass: a variant that beats control by the margin promotes —
// the file is replaced, the proposal lands applied with the @commit ref, the
// experiment closes promoted — and a drifted experiment aborts instead.
func TestLiveExperimentDecideAndPromote(t *testing.T) {
	f := newABFixture(t)
	libDir := t.TempDir()
	if err := os.MkdirAll(filepath.Join(libDir, "directives"), 0o755); err != nil {
		t.Fatal(err)
	}
	write := func(name, body string) {
		t.Helper()
		if err := os.WriteFile(filepath.Join(libDir, "directives", name+".md"), []byte("---\nmode: run\nmodel: haiku\n---\n"+body+"\n"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	git := func(args ...string) {
		t.Helper()
		out, err := exec.Command("git", append([]string{"-C", libDir, "-c", "user.name=t", "-c", "user.email=t@t"}, args...)...).CombinedOutput()
		if err != nil {
			t.Fatalf("git %v: %v %s", args, err, out)
		}
	}
	git("init", "-q", "-b", "main")
	write("subj", "control body {{objective}}")
	write("drifty", "drifty body {{objective}}")
	git("add", "-A")
	git("commit", "-q", "-m", "base")
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

	variant := "---\nmode: run\nmodel: haiku\n---\nwinning body {{objective}}\n"
	expID := openLiveExperiment(t, f.st, current, "directive:subj", "subj", variant)
	driftID := openLiveExperiment(t, f.st, current, "directive:drifty", "drifty", variant)

	// Facts: control 0/2 verified, v1 2/2 — v1 clears both margin clauses.
	seed := func(exp, arm string, verified bool, when time.Time) {
		a := f.attempt()
		state, pass := model.Failed, (*bool)(nil)
		if verified {
			v := true
			state, pass = model.Succeeded, &v
		}
		cost := 0.1
		f.write(func(tx *store.Tx) error {
			return tx.InsertFacts(actx(), &store.AttemptFacts{AttemptID: a.ID, TargetID: a.TargetID, Routine: "subj",
				Project: "default", Repository: "equitizr", Worker: testWorkerID, Executor: "claude-code",
				Model: "haiku", Mode: "run", Trigger: model.TriggerManual, Autonomy: model.AutonomyAuto,
				FinishedAt: when, State: state, VerificationPass: pass, CostUSD: &cost,
				Directive: "subj", LibraryCommit: directives.Head(libDir), ExperimentID: exp, Variant: arm})
		})
	}
	at := f.clock.Now().Add(-time.Hour)
	for i := 0; i < 2; i++ {
		seed(expID, "control", false, at.Add(time.Duration(i)*time.Second))
		seed(expID, "v1", true, at.Add(time.Duration(10+i)*time.Second))
	}

	// Drift the second experiment's subject before deciding.
	write("drifty", "changed underneath {{objective}}")
	git("add", "-A")
	git("commit", "-q", "-m", "human edit")
	if err := f.srv.promptsReload(); err != nil {
		t.Fatal(err)
	}

	f.srv.decideLiveExperiments(actx(), 0.20)

	got, err := f.st.GetExperiment(actx(), expID)
	if err != nil {
		t.Fatal(err)
	}
	if got.Status != store.ExperimentPromoted {
		t.Fatalf("experiment = %s (%s / %s)", got.Status, got.Error, got.Results)
	}
	var res liveResults
	if err := json.Unmarshal(got.Results, &res); err != nil || res.Winner != "v1" || res.ProposalID == "" || !strings.HasPrefix(res.AppliedRef, "directive:subj@") {
		t.Fatalf("results = %s (%v)", got.Results, err)
	}
	raw, _ := os.ReadFile(filepath.Join(libDir, "directives", "subj.md"))
	if !strings.Contains(string(raw), "winning body") {
		t.Fatalf("winner not applied: %s", raw)
	}
	ps, err := f.st.ListProposals(actx(), model.ProposalApplied)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, p := range ps {
		if p.ID == res.ProposalID && p.AppliedRef == res.AppliedRef {
			found = true
		}
	}
	if !found {
		t.Fatalf("promotion proposal not applied: %+v", ps)
	}

	drifted, err := f.st.GetExperiment(actx(), driftID)
	if err != nil {
		t.Fatal(err)
	}
	if drifted.Status != store.ExperimentAborted {
		t.Fatalf("drifted experiment = %s", drifted.Status)
	}

	// kept_control: a third subject whose variant does not clear the margin.
	write("keepy", "keepy body {{objective}}")
	git("add", "-A")
	git("commit", "-q", "-m", "keepy")
	if err := f.srv.promptsReload(); err != nil {
		t.Fatal(err)
	}
	keepID := openLiveExperiment(t, f.st, current, "directive:keepy", "keepy", variant)
	for i := 0; i < 2; i++ {
		seedK := func(arm string, verified bool) {
			a := f.attempt()
			state, pass := model.Failed, (*bool)(nil)
			if verified {
				v := true
				state, pass = model.Succeeded, &v
			}
			cost := 0.1
			f.write(func(tx *store.Tx) error {
				return tx.InsertFacts(actx(), &store.AttemptFacts{AttemptID: a.ID, TargetID: a.TargetID, Routine: "keepy",
					Project: "default", Repository: "equitizr", Worker: testWorkerID, Executor: "claude-code",
					Model: "haiku", Mode: "run", Trigger: model.TriggerManual, Autonomy: model.AutonomyAuto,
					FinishedAt: at.Add(time.Duration(20+i) * time.Second), State: state, VerificationPass: pass, CostUSD: &cost,
					Directive: "keepy", ExperimentID: keepID, Variant: arm})
			})
		}
		seedK("control", true)
		seedK("v1", true)
	}
	f.srv.decideLiveExperiments(actx(), 0.20)
	kept, err := f.st.GetExperiment(actx(), keepID)
	if err != nil {
		t.Fatal(err)
	}
	if kept.Status != store.ExperimentKeptControl {
		t.Fatalf("keepy experiment = %s (%s)", kept.Status, kept.Results)
	}
}

// A daemon restart rebuilds the assignment cache from the store; the rotation
// cursor is seeded from the count of already-stamped works so a deploy never
// resets arm rotation back to control (seen live: two deploys in a row gave
// consecutive reflect runs control twice).
func TestLiveAssignmentSurvivesRestart(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutineWith("rrdir", "base {{objective}}")
	openLiveExperiment(t, h.st, h.srv.promptLibrary(), "directive:rrdir", "rrdir",
		"---\nmode: run\nmodel: haiku\n---\nVARIANT {{objective}}\n")
	h.srv.refreshLiveExperiments(context.Background())

	h.run("rrdir") // control, cursor -> 1

	// Simulate the restart: wipe the in-memory cache, refresh from the store.
	h.srv.liveMu.Lock()
	h.srv.liveByDirective = map[string]*liveExperiment{}
	h.srv.liveByPersona = map[string]*liveExperiment{}
	h.srv.liveMu.Unlock()
	h.srv.refreshLiveExperiments(context.Background())

	created := h.run("rrdir")
	var comp struct {
		Variant string `json:"variant"`
	}
	if err := json.Unmarshal(created.Work.Composition, &comp); err != nil {
		t.Fatal(err)
	}
	if comp.Variant != "v1" {
		t.Fatalf("post-restart arm = %q, want v1 (rotation reset)", comp.Variant)
	}
}

// An experiment past decide_by with arms still short of min_runs closes
// inconclusive instead of waiting forever.
func TestLiveExperimentDeadlineInconclusive(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.createRoutineWith("latedir", "body {{objective}}")
	expID := openLiveExperimentBy(t, h.st, h.srv.promptLibrary(), "directive:latedir", "latedir",
		"---\nmode: run\nmodel: haiku\n---\nv {{objective}}\n", h.clock.Now().Add(-time.Hour))
	h.srv.decideLiveExperiments(context.Background(), 0.20)
	got, err := h.st.GetExperiment(context.Background(), expID)
	if err != nil || got.Status != store.ExperimentInconclusive {
		t.Fatalf("past-deadline experiment = %+v, %v", got, err)
	}
	var res liveResults
	if err := json.Unmarshal(got.Results, &res); err != nil || !strings.Contains(res.Reason, "deadline") {
		t.Fatalf("results = %s (%v)", got.Results, err)
	}
}

// The pollution rule: variant-arm facts are invisible to the A/B revert
// sweep — post-edit failures that all ran synthetic experiment content must
// not revert the library commit they happen to sit on.
func TestABRevertIgnoresVariantFacts(t *testing.T) {
	f := newABFixture(t)
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
		if err := os.WriteFile(filepath.Join(libDir, "directives", "triage-y.md"), []byte("---\nmode: run\nmodel: haiku\n---\n"+body+"\n"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	git("init", "-q", "-b", "main")
	write("old prompt {{objective}}")
	git("add", "-A")
	git("commit", "-q", "-m", "base")
	base := directives.Head(libDir)
	write("edited prompt {{objective}}")
	git("add", "-A")
	git("commit", "-q", "-m", "edit")
	edit := directives.Head(libDir)

	lib, err := directives.Load(libDir)
	if err != nil {
		t.Fatal(err)
	}
	f.srv.prompts = func() *directives.Library { return lib }
	f.srv.promptsReload = func() error { return nil }

	var pid string
	f.write(func(tx *store.Tx) error {
		p := &store.Proposal{Source: "retro:test", Kind: model.ProposalRoutine, Target: "routine:triage-y",
			After: json.RawMessage(`{"prompt":"new"}`), Rationale: "r", VerificationPlan: "A/B"}
		if err := tx.CreateProposal(actx(), p); err != nil {
			return err
		}
		pid = p.ID
		if _, err := tx.DecideProposal(actx(), p.ID, model.ProposalApproved, "human"); err != nil {
			return err
		}
		_, err := tx.MarkProposalApplied(actx(), p.ID, "directive:triage-y@"+edit)
		return err
	})

	seed := func(commit, variant string, verified bool, when time.Time) {
		a := f.attempt()
		state, pass := model.Failed, (*bool)(nil)
		if verified {
			v := true
			state, pass = model.Succeeded, &v
		}
		cost := 0.10
		f.write(func(tx *store.Tx) error {
			return tx.InsertFacts(actx(), &store.AttemptFacts{AttemptID: a.ID, TargetID: a.TargetID, Routine: "triage-y",
				Project: "default", Repository: "equitizr", Worker: testWorkerID, Executor: "claude-code",
				Model: "haiku", Mode: "run", Trigger: model.TriggerManual, Autonomy: model.AutonomyAuto,
				FinishedAt: when, State: state, VerificationPass: pass, CostUSD: &cost,
				Directive: "triage-y", LibraryCommit: commit, ExperimentID: "e-x", Variant: variant})
		})
	}
	at := f.clock.Now().Add(-time.Hour)
	// Five verified pre-edit control runs, five failed post-edit runs that
	// all carry a variant stamp — the failures are the experiment's, not the
	// edit's.
	for i := 0; i < 5; i++ {
		seed(base, "control", true, at.Add(time.Duration(i)*time.Second))
	}
	for i := 0; i < 5; i++ {
		seed(edit, "v1", false, at.Add(time.Duration(10+i)*time.Second))
	}

	f.srv.checkABReverts(actx(), config.ReflectionConfig{K: 5, Margin: 0.20})

	ps, err := f.st.ListProposals(actx(), model.ProposalReverted)
	if err != nil {
		t.Fatal(err)
	}
	for _, p := range ps {
		if p.ID == pid {
			t.Fatal("variant facts caused a revert")
		}
	}
	raw, _ := os.ReadFile(filepath.Join(libDir, "directives", "triage-y.md"))
	if !strings.Contains(string(raw), "edited prompt") {
		t.Fatalf("edit reverted on disk: %s", raw)
	}
}

func TestLiveExperimentAbortEndpoint(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.createRoutineWith("abdir", "body {{objective}}")
	expID := openLiveExperiment(t, h.st, h.srv.promptLibrary(), "directive:abdir", "abdir", "---\nmode: run\nmodel: haiku\n---\nv {{objective}}\n")
	h.call(http.MethodPost, "/api/v1/experiments/"+expID+"/abort", nil, nil, http.StatusOK)
	got, err := h.st.GetExperiment(context.Background(), expID)
	if err != nil || got.Status != store.ExperimentAborted {
		t.Fatalf("after abort = %+v, %v", got, err)
	}
	if status, _ := h.do(http.MethodPost, "/api/v1/experiments/"+expID+"/abort", nil, nil, ""); status == http.StatusOK {
		t.Fatal("second abort should not succeed")
	}
}
