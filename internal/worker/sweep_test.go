package worker

import (
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

// escapee starts a process that leaves its parent's process group with setsid
// — what a headless browser or a backgrounded preview server does — carrying
// tag in its environment. It returns the parent (a group leader of its own)
// and the escapee's pid.
func escapee(t *testing.T, tag string) (*exec.Cmd, int) {
	t.Helper()
	if _, err := exec.LookPath("setsid"); err != nil {
		t.Skip("setsid is not available")
	}
	pidFile := filepath.Join(t.TempDir(), "pid")
	cmd := exec.Command("sh", "-c", "setsid sh -c 'echo $$ > "+pidFile+"; exec sleep 300' & sleep 300")
	cmd.Env = append(os.Environ(), tag)
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
		_ = cmd.Wait()
	})
	pid := 0
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) && pid == 0 {
		b, err := os.ReadFile(pidFile)
		if err == nil {
			if n, err := strconv.Atoi(strings.TrimSpace(string(b))); err == nil {
				pid = n
			}
		}
		if pid == 0 {
			time.Sleep(20 * time.Millisecond)
		}
	}
	if pid == 0 {
		t.Fatal("the escaped process never reported its pid")
	}
	t.Cleanup(func() { _ = syscall.Kill(pid, syscall.SIGKILL) })
	return cmd, pid
}

// waitGone polls until pid is gone, so a test never depends on a sleep.
func waitGone(t *testing.T, pid int, within time.Duration) bool {
	t.Helper()
	deadline := time.Now().Add(within)
	for time.Now().Before(deadline) {
		if err := syscall.Kill(pid, 0); err == syscall.ESRCH {
			return true
		}
		time.Sleep(20 * time.Millisecond)
	}
	return false
}

func TestSweepKillsProcessThatLeftTheGroup(t *testing.T) {
	id := model.NewID()
	parent, escaped := escapee(t, AttemptEnv+"="+id)

	// The group kill an attempt does at exit cannot reach a process that left
	// the group: this is the leak the sweep exists for.
	start, err := ProcessStart(parent.Process.Pid)
	if err != nil {
		t.Fatal(err)
	}
	if err := KillGroup(parent.Process.Pid, start, killGrace); err != nil {
		t.Fatal(err)
	}
	if err := parent.Wait(); err == nil {
		t.Error("the parent should have died of a signal")
	}
	if err := syscall.Kill(escaped, 0); err != nil {
		t.Fatalf("the escaped process %d was expected to survive the group kill: %v", escaped, err)
	}

	n, err := Sweep(AttemptEnv, id, 2*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	if n == 0 {
		t.Fatalf("sweep found no process tagged %s=%s", AttemptEnv, id)
	}
	if !waitGone(t, escaped, 5*time.Second) {
		t.Errorf("escaped process %d survived the sweep", escaped)
	}
}

func TestSweepLeavesOtherTagsAlone(t *testing.T) {
	mine, theirs := model.NewID(), model.NewID()
	_, escaped := escapee(t, AttemptEnv+"="+theirs)

	n, err := Sweep(AttemptEnv, mine, 100*time.Millisecond)
	if err != nil {
		t.Fatal(err)
	}
	if n != 0 {
		t.Errorf("sweep of an unused tag found %d processes", n)
	}
	if err := syscall.Kill(escaped, 0); err != nil {
		t.Errorf("a process tagged with another attempt was signalled: %v", err)
	}
	// The prefix of a longer value must not match either.
	if n, err := Sweep(AttemptEnv, theirs[:8], 100*time.Millisecond); err != nil || n != 0 {
		t.Errorf("sweep of a tag prefix = %d, %v", n, err)
	}
	if n, err := Sweep(AttemptEnv, theirs, 5*time.Second); err != nil || n == 0 {
		t.Fatalf("sweep of the real tag = %d, %v", n, err)
	}
	if !waitGone(t, escaped, 5*time.Second) {
		t.Errorf("escaped process %d survived the sweep of its own tag", escaped)
	}
}

func TestProcessTagsAreNotInherited(t *testing.T) {
	env := PassthroughEnv([]string{"PATH=/usr/bin", WorkerEnv + "=outer", AttemptEnv + "=outer"}, ProcessTags("w1", "a1")...)
	for _, kv := range env {
		if kv == WorkerEnv+"=outer" || kv == AttemptEnv+"=outer" {
			t.Errorf("a marker from the parent environment leaked through: %q", kv)
		}
	}
	want := map[string]bool{WorkerEnv + "=w1": false, AttemptEnv + "=a1": false}
	for _, kv := range env {
		if _, ok := want[kv]; ok {
			want[kv] = true
		}
	}
	for kv, ok := range want {
		if !ok {
			t.Errorf("missing marker %q in %v", kv, env)
		}
	}
}
