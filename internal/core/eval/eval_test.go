package eval

import (
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"
)

// forgeBin is a freshly built binary: every case's daemon, worker, and
// fake-claude are real child processes of it (STYLE.md §5).
var forgeBin string

func TestMain(m *testing.M) {
	dir, err := os.MkdirTemp("", "forge-eval-test")
	if err != nil {
		panic(err)
	}
	forgeBin = filepath.Join(dir, "forge")
	build := exec.Command("go", "build", "-o", forgeBin, "forge/cmd/forge")
	build.Stderr = os.Stderr
	if err := build.Run(); err != nil {
		panic("build forge for eval tests: " + err.Error())
	}
	code := m.Run()
	if err := os.RemoveAll(dir); err != nil {
		panic(err)
	}
	os.Exit(code)
}

func writeCase(t *testing.T, dir, mode, name, body string) {
	t.Helper()
	caseDir := filepath.Join(dir, mode, name)
	if err := os.MkdirAll(caseDir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(caseDir, "eval.toml"), []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
}

func TestLoadCases(t *testing.T) {
	dir := t.TempDir()
	writeCase(t, dir, "run", "b-case", `
name = "b-case"
mode = "run"
prompt = "p"
fixture = "inventory"
[expect]
state = "succeeded"
`)
	writeCase(t, dir, "run", "a-case", `
name = "a-case"
mode = "run"
prompt = "p"
fixture = "failing"
autonomy = "auto"
[expect]
state = "failed"
failure_reason = "exit_nonzero"
`)
	cases, err := LoadCases(dir, "run")
	if err != nil {
		t.Fatal(err)
	}
	if len(cases) != 2 || cases[0].Name != "a-case" || cases[1].Name != "b-case" {
		t.Errorf("cases = %+v", cases)
	}
	if cases[0].Expect.FailureReason != "exit_nonzero" || cases[0].Autonomy != "auto" {
		t.Errorf("a-case = %+v", cases[0])
	}
}

func TestLoadCasesRefusals(t *testing.T) {
	refuse := func(name, body, wantErr string) {
		t.Helper()
		dir := t.TempDir()
		writeCase(t, dir, "run", name, body)
		if _, err := LoadCases(dir, "run"); err == nil {
			t.Errorf("%s: accepted (want error about %s)", name, wantErr)
		}
	}
	refuse("unknown-key", "name=\"x\"\nmode=\"run\"\nprompt=\"p\"\nfixture=\"f\"\nbogus=1\n[expect]\nstate=\"failed\"\n", "unknown key")
	refuse("bad-state", "name=\"x\"\nmode=\"run\"\nprompt=\"p\"\nfixture=\"f\"\n[expect]\nstate=\"great\"\n", "state")
	refuse("wrong-mode", "name=\"x\"\nmode=\"audit\"\nprompt=\"p\"\nfixture=\"f\"\n[expect]\nstate=\"failed\"\n", "mode mismatch")
	refuse("bad-repo", "name=\"x\"\nmode=\"run\"\nprompt=\"p\"\nfixture=\"f\"\nrepo_fixture=\"weird\"\n[expect]\nstate=\"failed\"\n", "repo_fixture")
	if _, err := LoadCases(t.TempDir(), "run"); err == nil {
		t.Error("empty cases dir accepted")
	}
}

func viewOf(t *testing.T, raw string) *taskView {
	t.Helper()
	var v taskView
	if err := json.Unmarshal([]byte(raw), &v); err != nil {
		t.Fatal(err)
	}
	return &v
}

func TestScore(t *testing.T) {
	yes := true
	succeeded := `{"work":{"id":"w"},"state":"succeeded","targets":[{"state":"succeeded"}],
		"attempts":[{"num_turns":2,"cost_usd":0.01,"verification_passed":true,
		"result":{"claims":[{"claim":"a"},{"claim":"b"}]}}]}`
	failed := `{"work":{"id":"w"},"state":"failed","targets":[{"state":"failed","failure_reason":"ambiguity_at_auto"}],
		"attempts":[{"num_turns":1}]}`
	cases := []struct {
		name   string
		expect Expect
		view   string
		pass   bool
	}{
		{"exact success", Expect{State: "succeeded", MinClaims: 2, Verified: &yes}, succeeded, true},
		{"state mismatch", Expect{State: "failed"}, succeeded, false},
		{"claims short", Expect{State: "succeeded", MinClaims: 3}, succeeded, false},
		{"failure with reason", Expect{State: "failed", FailureReason: "ambiguity_at_auto"}, failed, true},
		{"wrong reason", Expect{State: "failed", FailureReason: "timeout"}, failed, false},
		{"verified wanted but absent", Expect{State: "failed", Verified: &yes}, failed, false},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			pass, details := score(Case{Expect: c.expect}, viewOf(t, c.view))
			if pass != c.pass {
				t.Errorf("pass = %v (details %q)", pass, details)
			}
			if !pass && details == "" {
				t.Error("a failing score must say why")
			}
		})
	}
}

// TestRunGoldenCases runs the shipped evals/run cases end to end: per case an
// isolated FORGE_HOME, a daemon child, its worker, and fake-claude replaying
// the fixture. This is the eval engine's own eval — every shipped case must
// pass, scoring 1.0.
func TestRunGoldenCases(t *testing.T) {
	casesDir, err := filepath.Abs("../../../evals")
	if err != nil {
		t.Fatal(err)
	}
	fixturesDir, err := filepath.Abs("../../../testdata/fixtures")
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()
	rep, err := Run(ctx, Options{
		Mode: "run", CasesDir: casesDir, FixturesDir: fixturesDir,
		ForgeBin: forgeBin, WorkDir: t.TempDir(), Timeout: 90 * time.Second,
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(rep.Cases) != 4 {
		t.Errorf("cases = %d, want 4", len(rep.Cases))
	}
	for _, r := range rep.Cases {
		if !r.Pass {
			t.Errorf("case %s failed: state %s reason %s details %q", r.Name, r.State, r.FailureReason, r.Details)
		}
		if r.Pass && (r.Turns == 0 && r.State == "succeeded") {
			t.Errorf("case %s: a succeeded case must report turns", r.Name)
		}
	}
	if rep.Score != 1.0 {
		t.Errorf("score = %v, want 1.0", rep.Score)
	}
}
