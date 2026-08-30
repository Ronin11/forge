package main

import (
	"bytes"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

// TestMain lets the test binary act as the forge binary when FORGE_TEST_MAIN is
// set, so fake-claude is exercised as a real child process with its own stdin,
// cwd, and exit code — the way the worker will run it.
func TestMain(m *testing.M) {
	if os.Getenv("FORGE_TEST_MAIN") == "1" {
		main()
		return
	}
	os.Exit(m.Run())
}

func fixturesDir(t *testing.T) string {
	t.Helper()
	abs, err := filepath.Abs("../../testdata/fixtures")
	if err != nil {
		t.Fatal(err)
	}
	return abs
}

// runForge execs the test binary as `forge <args>` in dir with prompt on stdin.
func runForge(t *testing.T, dir, prompt string, args ...string) (stdout, stderr string, code int) {
	t.Helper()
	cmd := exec.Command(os.Args[0], args...)
	cmd.Dir = dir
	cmd.Stdin = strings.NewReader(prompt)
	cmd.Env = append(os.Environ(), "FORGE_TEST_MAIN=1", "FORGE_HOME="+t.TempDir())
	var out, errb bytes.Buffer
	cmd.Stdout, cmd.Stderr = &out, &errb
	err := cmd.Run()
	if exit, ok := err.(*exec.ExitError); ok {
		code = exit.ExitCode()
	} else if err != nil {
		t.Fatal(err)
	}
	return out.String(), errb.String(), code
}

func jsonLines(t *testing.T, stdout string) []map[string]any {
	t.Helper()
	var out []map[string]any
	for i, l := range strings.Split(strings.TrimSpace(stdout), "\n") {
		var m map[string]any
		if err := json.Unmarshal([]byte(l), &m); err != nil {
			t.Fatalf("line %d is not JSON: %q", i+1, l)
		}
		out = append(out, m)
	}
	return out
}

// obj, str, and arr read typed values out of decoded JSON, failing the test on a
// shape mismatch instead of panicking on an unchecked assertion.
func obj(t *testing.T, m map[string]any, key string) map[string]any {
	t.Helper()
	v, ok := m[key].(map[string]any)
	if !ok {
		t.Fatalf("%q is not an object: %v", key, m[key])
	}
	return v
}

func str(t *testing.T, m map[string]any, key string) string {
	t.Helper()
	v, ok := m[key].(string)
	if !ok {
		t.Fatalf("%q is not a string: %v", key, m[key])
	}
	return v
}

func arr(t *testing.T, m map[string]any, key string) []map[string]any {
	t.Helper()
	raw, ok := m[key].([]any)
	if !ok {
		t.Fatalf("%q is not an array: %v", key, m[key])
	}
	out := make([]map[string]any, 0, len(raw))
	for _, e := range raw {
		o, ok := e.(map[string]any)
		if !ok {
			t.Fatalf("%q element is not an object: %v", key, e)
		}
		out = append(out, o)
	}
	return out
}

func lastResult(t *testing.T, lines []map[string]any) map[string]any {
	t.Helper()
	for i := len(lines) - 1; i >= 0; i-- {
		if lines[i]["type"] == "result" {
			return lines[i]
		}
	}
	t.Fatal("no result line")
	return nil
}

func TestFakeClaudeInventoryIgnoresRealFlags(t *testing.T) {
	stdout, stderr, code := runForge(t, t.TempDir(), "list the files",
		"fake-claude", "--print", "--verbose", "--output-format", "stream-json", "--dangerously-skip-permissions",
		"--model", "haiku", "--max-turns", "5", "--mcp-config", "/nowhere.json", "--allowedTools", "Bash,Read",
		"--json-schema", `{"type":"object"}`, "--fixture", filepath.Join(fixturesDir(t), "inventory"))
	if code != 0 {
		t.Fatalf("exit %d, stderr %q", code, stderr)
	}
	if !strings.Contains(stderr, "fake-claude: fixture inventory") {
		t.Errorf("stderr = %q", stderr)
	}
	lines := jsonLines(t, stdout)
	init := lines[0]
	session := str(t, init, "session_id")
	if init["type"] != "system" || len(session) != 36 || strings.Contains(stdout, "{{SESSION}}") {
		t.Errorf("init = %v", init)
	}
	res := lastResult(t, lines)
	if res["session_id"] != session || res["is_error"] != false || res["structured_output"] == nil {
		t.Errorf("result = %v", res)
	}
}

func TestFakeClaudeCommitWritesAndCommits(t *testing.T) {
	dir := t.TempDir()
	for _, args := range [][]string{{"init", "-q", "-b", "master"}, {"commit", "-q", "--allow-empty", "-m", "root"}} {
		cmd := exec.Command("git", args...)
		cmd.Dir = dir
		cmd.Env = append(os.Environ(), "GIT_AUTHOR_NAME=t", "GIT_AUTHOR_EMAIL=t@x", "GIT_COMMITTER_NAME=t", "GIT_COMMITTER_EMAIL=t@x")
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v %s", args, err, out)
		}
	}
	t.Setenv("FORGE_FAKE_FIXTURE", filepath.Join(fixturesDir(t), "commit"))
	stdout, stderr, code := runForge(t, dir, "create the file", "fake-claude", "--print")
	if code != 0 {
		t.Fatalf("exit %d: %s", code, stderr)
	}
	if data, err := os.ReadFile(filepath.Join(dir, "FORGE_SMOKE.txt")); err != nil || string(data) != "2026-08-30\n" {
		t.Errorf("file: %q %v", data, err)
	}
	log := exec.Command("git", "log", "--oneline")
	log.Dir = dir
	out, err := log.Output()
	if err != nil || !strings.Contains(string(out), "chore: forge smoke test") {
		t.Errorf("git log = %q %v", out, err)
	}
	if res := lastResult(t, jsonLines(t, stdout)); res["num_turns"] != float64(3) {
		t.Errorf("result = %v", res)
	}
}

func TestFakeClaudeFailingExitsNonZero(t *testing.T) {
	stdout, _, code := runForge(t, t.TempDir(), "p", "fake-claude", "--fixture", filepath.Join(fixturesDir(t), "failing"))
	if code != 1 {
		t.Errorf("exit = %d", code)
	}
	if res := lastResult(t, jsonLines(t, stdout)); res["is_error"] != true || res["subtype"] != "error_max_turns" {
		t.Errorf("result = %v", res)
	}
}

func TestFakeClaudeNeedsInputAndResume(t *testing.T) {
	fx := filepath.Join(fixturesDir(t), "needs-input")
	stdout, _, code := runForge(t, t.TempDir(), "improve the README", "fake-claude", "--fixture", fx)
	if code != 0 {
		t.Fatalf("exit %d", code)
	}
	lines := jsonLines(t, stdout)
	if len(lines) != 7 { // 6 script lines + synthesised result
		t.Fatalf("got %d lines, want 7", len(lines))
	}
	res := lastResult(t, lines)
	if so := obj(t, res, "structured_output"); so["needs_input"] == nil {
		t.Fatalf("no needs_input in %v", res)
	}
	var text map[string]any
	if err := json.Unmarshal([]byte(str(t, res, "result")), &text); err != nil || text["needs_input"] == nil {
		t.Errorf("result text is not the envelope: %v %v", res["result"], err)
	}
	session := str(t, res, "session_id")
	stdout, _, code = runForge(t, t.TempDir(), "docs/README.md", "fake-claude", "--fixture", fx, "--resume", session)
	if code != 0 {
		t.Fatalf("resume exit %d", code)
	}
	lines = jsonLines(t, stdout)
	if lines[0]["session_id"] != session || len(lines) != 5 {
		t.Errorf("resume: %d lines, session %v", len(lines), lines[0]["session_id"])
	}
	if res := lastResult(t, lines); res["session_id"] != session || obj(t, res, "structured_output")["needs_input"] != nil {
		t.Errorf("resume result = %v", res)
	}
}

func TestFakeClaudeToolErrorEmitsRateLimit(t *testing.T) {
	stdout, _, code := runForge(t, t.TempDir(), "p", "fake-claude", "--fixture", filepath.Join(fixturesDir(t), "tool-error"))
	if code != 0 {
		t.Fatalf("exit %d", code)
	}
	lines := jsonLines(t, stdout)
	var sawRate, sawErr bool
	for _, l := range lines {
		if l["type"] == "rate_limit_event" {
			sawRate = true
			windows := obj(t, obj(t, l, "rate_limit_info"), "unifiedWindows")
			if obj(t, windows, "five_hour")["utilization"] != 0.41 {
				t.Errorf("rate limit = %v", windows)
			}
		}
		if l["type"] == "user" {
			for _, b := range arr(t, obj(t, l, "message"), "content") {
				if b["is_error"] == true {
					sawErr = true
				}
			}
		}
	}
	if !sawRate || !sawErr || lines[len(lines)-2]["type"] != "rate_limit_event" {
		t.Errorf("rate=%v err=%v; rate_limit_event must precede the result", sawRate, sawErr)
	}
}

func TestFakeClaudeUsageErrors(t *testing.T) {
	if _, stderr, code := runForge(t, t.TempDir(), "", "fake-claude", "--fixture", "/no/such/fixture"); code != 2 || !strings.Contains(stderr, "not a directory") {
		t.Errorf("missing fixture: %d %q", code, stderr)
	}
	if _, stderr, code := runForge(t, t.TempDir(), "", "fake-claude"); code != 2 || !strings.Contains(stderr, "--fixture") {
		t.Errorf("no fixture: %d %q", code, stderr)
	}
	// The timeout fixture must at least load; never wait for its 10-minute delay.
	fx, err := loadFixture(filepath.Join(fixturesDir(t), "timeout"), false)
	if err != nil || len(fx.meta.LineDelays) != 1 || fx.meta.LineDelays[0].DelayMS != 600000 {
		t.Errorf("timeout fixture: %+v %v", fx, err)
	}
}
