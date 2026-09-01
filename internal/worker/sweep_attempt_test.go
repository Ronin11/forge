package worker

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	"forge/internal/model"
)

// leakyExecutor rewrites the fixture's executor so the agent leaves a process
// behind in a session of its own — a headless browser or a preview server,
// reduced to its essentials — before running fake-claude as usual. It returns
// the file the leaked process writes its pid to.
func leakyExecutor(t *testing.T, f *runnerFixture) string {
	t.Helper()
	if _, err := exec.LookPath("setsid"); err != nil {
		t.Skip("setsid is not available")
	}
	pidFile := filepath.Join(t.TempDir(), "leaked.pid")
	script := "setsid sh -c 'echo $$ > " + pidFile + "; exec sleep 300' >/dev/null 2>&1 & exec \"$0\" \"$@\""
	cfg := map[string]ExecutorConfig{"fake-claude": {
		Command: append([]string{"sh", "-c", script}, f.runner.cfg.Executors["fake-claude"].Command...),
		Output:  "claude-stream-json", Capabilities: []string{"resume"},
	}}
	execs, err := ExecutorsFromConfig(cfg)
	if err != nil {
		t.Fatal(err)
	}
	f.runner.executors = execs
	return pidFile
}

// leakedPID waits for the leaked process to report itself; it is started
// before the executor, so by the time the attempt has finished it is written.
func leakedPID(t *testing.T, pidFile string) int {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		if b, err := os.ReadFile(pidFile); err == nil {
			if pid, err := strconv.Atoi(strings.TrimSpace(string(b))); err == nil {
				t.Cleanup(func() { _ = syscall.Kill(pid, syscall.SIGKILL) })
				return pid
			}
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("no pid in %s: the executor never leaked a process", pidFile)
	return 0
}

func TestAttemptKillsProcessesTheAgentLeftRunning(t *testing.T) {
	f := newRunnerFixture(t, "inventory")
	pidFile := leakyExecutor(t, f)
	c := f.claim(model.AutonomyAuto)
	req := f.run(c)
	if req.State != model.Succeeded {
		t.Fatalf("state = %s (%s)", req.State, req.FailureReason)
	}
	pid := leakedPID(t, pidFile)
	if !waitGone(t, pid, 5*time.Second) {
		t.Errorf("process %d, left in its own session by the agent, survived the attempt", pid)
	}
}

// TestWorkerShutdownKillsLeftoverProcesses covers the second half of the rule:
// whatever an attempt could not clean up itself dies when the worker stops.
func TestWorkerShutdownKillsLeftoverProcesses(t *testing.T) {
	w, _, _ := newReconcileWorker(t)
	child := exec.Command("sh", "-c", "sleep 300")
	child.Env = append(os.Environ(), WorkerEnv+"="+w.ID(), AttemptEnv+"="+model.NewID())
	child.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := child.Start(); err != nil {
		t.Fatal(err)
	}
	pid := child.Process.Pid
	t.Cleanup(func() {
		_ = syscall.Kill(pid, syscall.SIGKILL)
		_ = child.Wait()
	})

	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if err := w.Run(ctx); err != nil {
		t.Fatal(err)
	}
	if !waitGone(t, pid, 5*time.Second) {
		t.Errorf("process %d tagged with this worker survived its shutdown", pid)
	}
}

// TestReconcileKillsLeftoverProcessesOfACrashedWorker is the restart path: the
// agent's group is gone, but what it left in another session is not.
func TestReconcileKillsLeftoverProcessesOfACrashedWorker(t *testing.T) {
	w, gf, d := newReconcileWorker(t)
	m, agent := orphanedAttempt(t, w, gf, false)
	d.states[m.AttemptID] = &AttemptState{AttemptID: m.AttemptID, TargetState: "running"}
	stray := exec.Command("sh", "-c", "sleep 300")
	stray.Env = append(os.Environ(), AttemptEnv+"="+m.AttemptID)
	stray.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := stray.Start(); err != nil {
		t.Fatal(err)
	}
	pid := stray.Process.Pid
	t.Cleanup(func() {
		_ = syscall.Kill(pid, syscall.SIGKILL)
		_ = stray.Wait()
	})

	if err := w.reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if err := agent.Wait(); err == nil {
		t.Error("the orphaned agent should have died of a signal")
	}
	if !waitGone(t, pid, 5*time.Second) {
		t.Errorf("process %d left by the crashed worker's attempt survived reconcile", pid)
	}
}
