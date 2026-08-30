package worker

import (
	"bytes"
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"
)

type lineSink struct {
	mu    sync.Mutex
	lines []string
}

func (l *lineSink) add(b []byte) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.lines = append(l.lines, string(b))
}

func (l *lineSink) get() []string {
	l.mu.Lock()
	defer l.mu.Unlock()
	return append([]string(nil), l.lines...)
}

func shell(script string) *exec.Cmd { return exec.Command("sh", "-c", script) }

func TestLaunchNormalExit(t *testing.T) {
	out := filepath.Join(t.TempDir(), "out.log")
	var stdout, stderr lineSink
	p, err := Launch(context.Background(), LaunchSpec{
		Cmd: shell(`echo one; echo two; echo err >&2; echo three; exit 3`), Timeout: 10 * time.Second,
		OutputPath: out, OnStdout: stdout.add, OnStderr: stderr.add,
	})
	if err != nil {
		t.Fatal(err)
	}
	if p.PID() <= 0 || p.PIDStart() <= 0 {
		t.Errorf("identity not recorded: %d %d", p.PID(), p.PIDStart())
	}
	st := p.Wait()
	if st.Code != 3 || st.TimedOut || st.Stopped != "" {
		t.Errorf("status = %+v", st)
	}
	if got := strings.Join(stdout.get(), ","); got != "one,two,three" {
		t.Errorf("stdout = %q", got)
	}
	if got := stderr.get(); len(got) != 1 || got[0] != "err" || !strings.Contains(st.StderrTail, "err") {
		t.Errorf("stderr = %v tail=%q", got, st.StderrTail)
	}
	data, err := os.ReadFile(out)
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"one", "two", "three", "err"} {
		if !bytes.Contains(data, []byte(want)) {
			t.Errorf("output file missing %q: %q", want, data)
		}
	}
	if st.OutputBytes == 0 || st.Truncated {
		t.Errorf("output stats: %+v", st)
	}
	if err := p.Stop("late", 0); err != nil {
		t.Errorf("Stop after exit: %v", err)
	}
}

func TestLaunchPromptOnStdin(t *testing.T) {
	var stdout lineSink
	p, err := Launch(context.Background(), LaunchSpec{Cmd: shell(`cat`), Prompt: "hello prompt\n", Timeout: 10 * time.Second, OnStdout: stdout.add})
	if err != nil {
		t.Fatal(err)
	}
	if st := p.Wait(); st.Code != 0 {
		t.Fatalf("status = %+v", st)
	}
	if got := stdout.get(); len(got) != 1 || got[0] != "hello prompt" {
		t.Errorf("stdout = %v", got)
	}
}

func TestLaunchTimeoutKillsProcessGroup(t *testing.T) {
	var stdout lineSink
	p, err := Launch(context.Background(), LaunchSpec{
		Cmd: shell(`sleep 300 & echo $!; sleep 300`), Timeout: 300 * time.Millisecond, OnStdout: stdout.add,
	})
	if err != nil {
		t.Fatal(err)
	}
	st := p.Wait()
	if !st.TimedOut || (st.Code != 137 && st.Code != 143 && st.Code != 128+15) {
		t.Errorf("status = %+v", st)
	}
	lines := stdout.get()
	if len(lines) != 1 {
		t.Fatalf("expected the grandchild pid on stdout, got %v", lines)
	}
	pid, err := strconv.Atoi(strings.TrimSpace(lines[0]))
	if err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		if err := syscall.Kill(pid, 0); err == syscall.ESRCH {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Errorf("grandchild %d survived the group kill", pid)
}

func TestStopEscalatesToKill(t *testing.T) {
	p, err := Launch(context.Background(), LaunchSpec{Cmd: shell(`trap '' TERM; sleep 300`), Timeout: 30 * time.Second})
	if err != nil {
		t.Fatal(err)
	}
	time.Sleep(50 * time.Millisecond) // let sh install the trap; bounded, not a sync
	if err := p.Stop("cancelled", 100*time.Millisecond); err != nil {
		t.Fatal(err)
	}
	st := p.Wait()
	if st.Stopped != "cancelled" || st.Signal != "SIGKILL" || st.Code != 137 {
		t.Errorf("status = %+v", st)
	}
}

func TestLongLineAndBoundedMirror(t *testing.T) {
	var stdout lineSink
	p, err := Launch(context.Background(), LaunchSpec{Cmd: shell(`head -c 2097152 /dev/zero | tr '\0' 'x'; echo; echo tail`), Timeout: 10 * time.Second, OnStdout: stdout.add})
	if err != nil {
		t.Fatal(err)
	}
	if st := p.Wait(); st.Code != 0 {
		t.Fatalf("status = %+v", st)
	}
	lines := stdout.get()
	if len(lines) < 2 || len(lines[0]) > MaxLineBytes || lines[len(lines)-1] != "tail" {
		t.Errorf("long line handling: %d lines, first %d bytes, last %q", len(lines), len(lines[0]), lines[len(lines)-1])
	}
	out := filepath.Join(t.TempDir(), "out.log")
	p, err = Launch(context.Background(), LaunchSpec{Cmd: shell(`head -c 102400 /dev/zero | tr '\0' 'y'`), Timeout: 10 * time.Second, OutputPath: out, MaxOutput: 4096})
	if err != nil {
		t.Fatal(err)
	}
	st := p.Wait()
	info, err := os.Stat(out)
	if err != nil {
		t.Fatal(err)
	}
	if !st.Truncated || st.OutputBytes < 102400 || info.Size() > 4096+256 {
		t.Errorf("mirror bound: truncated=%v produced=%d size=%d", st.Truncated, st.OutputBytes, info.Size())
	}
}

func TestProcessIdentityAndKillGroupRefusal(t *testing.T) {
	self := os.Getpid()
	start, err := ProcessStart(self)
	if err != nil || start <= 0 {
		t.Fatalf("ProcessStart(self) = %d, %v", start, err)
	}
	if alive, err := ProcessAlive(self, start); err != nil || !alive {
		t.Errorf("self alive = %v, %v", alive, err)
	}
	if alive, err := ProcessAlive(self, start+1); err != nil || alive {
		t.Errorf("self with wrong start = %v, %v", alive, err)
	}
	cmd := shell("true")
	if err := cmd.Run(); err != nil {
		t.Fatal(err)
	}
	if alive, err := ProcessAlive(cmd.Process.Pid, 1); alive {
		t.Errorf("exited process reported alive (%v)", err)
	}
	if err := KillGroup(self, start+1, 0); err == nil {
		t.Fatal("KillGroup accepted a wrong identity (and would have killed the test)")
	}
	if err := KillGroup(999999, 1, 0); err != nil {
		t.Errorf("KillGroup on a dead pid should be success: %v", err)
	}
}
