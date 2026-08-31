package main

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

// TestFakeClaudeRateLimitedFixture checks positioned [[rate_limit_events]]
// interleave with the script and the final rate_limit_event still precedes the
// result — the stream budget tests replay to drive utilisation.
func TestFakeClaudeRateLimitedFixture(t *testing.T) {
	stdout, stderr, code := runForge(t, t.TempDir(), "p", "fake-claude", "--fixture", filepath.Join(fixturesDir(t), "rate-limited"))
	if code != 0 {
		t.Fatalf("exit %d: %s", code, stderr)
	}
	lines := jsonLines(t, stdout)
	var fiveHour []float64
	var lastRateIdx, resultIdx int
	for i, l := range lines {
		switch l["type"] {
		case "rate_limit_event":
			windows := obj(t, obj(t, l, "rate_limit_info"), "unifiedWindows")
			v, ok := obj(t, windows, "five_hour")["utilization"].(float64)
			if !ok {
				t.Fatalf("utilization is not a number: %v", windows)
			}
			fiveHour = append(fiveHour, v)
			lastRateIdx = i
		case "result":
			resultIdx = i
		}
	}
	if len(fiveHour) != 3 || fiveHour[0] != 0.35 || fiveHour[1] != 0.62 || fiveHour[2] != 0.81 {
		t.Errorf("five_hour utilisations = %v, want [0.35 0.62 0.81]", fiveHour)
	}
	if lastRateIdx+1 != resultIdx {
		t.Errorf("final rate_limit_event at %d must directly precede the result at %d", lastRateIdx, resultIdx)
	}
	// The positioned events land right after their lines: line 2 is index 1,
	// so the first rate event is index 2.
	if lines[2]["type"] != "rate_limit_event" || lines[4]["type"] != "rate_limit_event" {
		t.Errorf("positioned events misplaced: types = %v %v", lines[2]["type"], lines[4]["type"])
	}
}

// TestFakeClaudeCommitFilesWritesWithoutCommitting checks the fixture leaves
// the cwd dirty — files present, nothing committed — so git_inspect sees
// exactly what the emitted result claims.
func TestFakeClaudeCommitFilesWritesWithoutCommitting(t *testing.T) {
	dir := t.TempDir()
	for _, args := range [][]string{{"init", "-q"}, {"commit", "-q", "--allow-empty", "-m", "root"}} {
		cmd := exec.Command("git", args...)
		cmd.Dir = dir
		cmd.Env = append(os.Environ(), "GIT_AUTHOR_NAME=t", "GIT_AUTHOR_EMAIL=t@x", "GIT_COMMITTER_NAME=t", "GIT_COMMITTER_EMAIL=t@x")
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v %s", args, err, out)
		}
	}
	stdout, stderr, code := runForge(t, dir, "write the plan", "fake-claude", "--fixture", filepath.Join(fixturesDir(t), "commit-files"))
	if code != 0 {
		t.Fatalf("exit %d: %s", code, stderr)
	}
	for path, want := range map[string]string{
		"FORGE_NOTES.md": "# Notes\n\nWritten by the commit-files fixture.\n",
		"docs/PLAN.md":   "# Plan\n\n1. Write files.\n2. Claim them in the result.\n",
	} {
		if data, err := os.ReadFile(filepath.Join(dir, path)); err != nil || string(data) != want {
			t.Errorf("%s: %q %v", path, data, err)
		}
	}
	status := exec.Command("git", "status", "--porcelain")
	status.Dir = dir
	out, err := status.Output()
	if err != nil || !strings.Contains(string(out), "FORGE_NOTES.md") || !strings.Contains(string(out), "docs/") {
		t.Errorf("worktree should be dirty with both files: %q %v", out, err)
	}
	log := exec.Command("git", "log", "--oneline")
	log.Dir = dir
	if out, err := log.Output(); err != nil || strings.Count(strings.TrimSpace(string(out)), "\n") != 0 {
		t.Errorf("nothing may be committed beyond the root: %q %v", out, err)
	}
	res := lastResult(t, jsonLines(t, stdout))
	if changes := arr(t, obj(t, res, "structured_output"), "changes"); len(changes) != 2 {
		t.Errorf("result claims %d changes, want 2", len(changes))
	}
}

// TestFakeClaudeMetaRejectsBadRateLimitLines: a positioned event pointing past
// the script is a fixture bug and must fail loudly as a usage error.
func TestFakeClaudeMetaRejectsBadRateLimitLines(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "script.jsonl"), []byte("{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"{{SESSION}}\"}\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	meta := "exit_code = 0\n[[rate_limit_events]]\nat_line = 99\nfive_hour = 0.5\nseven_day = 0.1\n"
	if err := os.WriteFile(filepath.Join(dir, "meta.toml"), []byte(meta), 0o600); err != nil {
		t.Fatal(err)
	}
	_, stderr, code := runForge(t, t.TempDir(), "", "fake-claude", "--fixture", dir)
	if code != 2 || !strings.Contains(stderr, "rate_limit_events") {
		t.Errorf("exit %d stderr %q, want usage error naming rate_limit_events", code, stderr)
	}
	meta = "exit_code = 0\n[[rate_limit_events]]\nat_line = 1\nfive_hour = -0.5\nseven_day = 0.1\n"
	if err := os.WriteFile(filepath.Join(dir, "meta.toml"), []byte(meta), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, stderr, code := runForge(t, t.TempDir(), "", "fake-claude", "--fixture", dir); code != 2 || !strings.Contains(stderr, "negative") {
		t.Errorf("exit %d stderr %q, want usage error about negative utilisation", code, stderr)
	}
}
