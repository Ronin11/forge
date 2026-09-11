package worker

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// TestRunChecksKillsWhatTheCheckLeftRunning is the preview-server case: a build
// that backgrounds a server both keeps the check's stdout open and outlives it.
// The check must neither wait for it nor leave it running.
func TestRunChecksKillsWhatTheCheckLeftRunning(t *testing.T) {
	dir := t.TempDir()
	pidFile := filepath.Join(dir, "server.pid")
	ft := &ForgeToml{
		Checks:        map[string][]string{"build": {"sh", "-c", "sleep 300 & echo $! > " + pidFile + "; echo built"}},
		CheckTimeouts: map[string]int{"build": 30},
	}
	start := time.Now()
	results := RunChecks(context.Background(), dir, ft, os.Environ())
	elapsed := time.Since(start)
	if len(results) != 1 || !results[0].Passed {
		t.Fatalf("results = %+v", results)
	}
	if !strings.Contains(results[0].OutputTail, "built") {
		t.Errorf("output tail = %q", results[0].OutputTail)
	}
	if elapsed > 10*time.Second {
		t.Errorf("the check waited %s for the process it backgrounded", elapsed)
	}
	pid := leakedPID(t, pidFile)
	if !waitGone(t, pid, 5*time.Second) {
		t.Errorf("process %d, backgrounded by the check, survived it", pid)
	}
}

// TestRunChecksRunsSetupFirst is the fresh-clone case: "build" sorts before
// "setup", so in plain name order the build runs against a tree nothing has
// installed into yet. The rest keep name order.
func TestRunChecksRunsSetupFirst(t *testing.T) {
	dir := t.TempDir()
	needsInstall := []string{"sh", "-c", "test -f installed"}
	ft := &ForgeToml{Checks: map[string][]string{
		"typecheck": needsInstall,
		"setup":     {"sh", "-c", "touch installed"},
		"build":     needsInstall,
		"test":      needsInstall,
	}}
	results := RunChecks(context.Background(), dir, ft, os.Environ())
	var order []string
	for _, r := range results {
		order = append(order, r.Check)
		if !r.Passed {
			t.Errorf("check %s failed: %q", r.Check, r.OutputTail)
		}
	}
	if got := strings.Join(order, ","); got != "setup,build,test,typecheck" {
		t.Errorf("order = %s, want setup,build,test,typecheck", got)
	}
}

// TestRunChecksKeepsTheOutputTailBounded: a check that prints more than the
// tail Forge keeps is truncated to its end, where the failures are.
func TestRunChecksKeepsTheOutputTailBounded(t *testing.T) {
	dir := t.TempDir()
	ft := &ForgeToml{
		Checks:        map[string][]string{"noisy": {"sh", "-c", "head -c 200000 /dev/zero | tr '\\0' 'x'; echo; echo --- FAIL: TestLast; exit 1"}},
		CheckTimeouts: map[string]int{"noisy": 30},
	}
	results := RunChecks(context.Background(), dir, ft, os.Environ())
	if len(results) != 1 || results[0].Passed || results[0].ExitCode != 1 {
		t.Fatalf("results = %+v", results[0])
	}
	if n := len(results[0].OutputTail); n > checkTailBytes {
		t.Errorf("output tail = %d bytes, bound is %d", n, checkTailBytes)
	}
	if !strings.HasSuffix(strings.TrimSpace(results[0].OutputTail), "--- FAIL: TestLast") {
		t.Errorf("the end of the output was not kept: %q", results[0].OutputTail[max(0, len(results[0].OutputTail)-80):])
	}
	if len(results[0].Failing) != 1 || results[0].Failing[0] != "TestLast" {
		t.Errorf("failing tests = %v", results[0].Failing)
	}
}
