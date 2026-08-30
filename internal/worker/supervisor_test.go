package worker

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

func writeScript(t *testing.T, body string) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), "script.sh")
	if err := os.WriteFile(p, []byte("#!/bin/sh\n"+body), 0o700); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestRunTimeoutKillsProcessGroup(t *testing.T) {
	// The script spawns a grandchild sleep that must die with the group.
	script := writeScript(t, "echo started\nsleep 300 &\nsleep 300\n")
	var pid int
	var events []string
	out := Run(context.Background(), RunSpec{
		Argv: []string{script}, Dir: t.TempDir(), Timeout: 500 * time.Millisecond,
		Parser:  &linesParser{keep: 10},
		OnEvent: func(kind, msg string) { events = append(events, kind+":"+msg) },
		OnStart: func(p int, identity string) error {
			pid = p
			if identity == "" {
				t.Error("empty process identity")
			}
			return nil
		},
	})
	if out.Reason != "timeout" {
		t.Fatalf("reason %q err %v", out.Reason, out.Err)
	}
	if out.ExitCode == 0 {
		t.Error("exit code 0 after kill")
	}
	if len(events) == 0 || events[0] != "stdout:started" {
		t.Errorf("events %v", events)
	}
	deadline := time.Now().Add(2 * time.Second)
	for processGroupAlive(pid) && time.Now().Before(deadline) {
		time.Sleep(20 * time.Millisecond)
	}
	if processGroupAlive(pid) {
		t.Fatalf("process group %d still alive after timeout", pid)
	}
}

func TestRunCancelKillsProcessGroup(t *testing.T) {
	script := writeScript(t, "trap '' TERM\nsleep 300 &\nwait\n") // ignores SIGTERM: forces SIGKILL path
	ctx, cancel := context.WithCancel(context.Background())
	go func() { time.Sleep(200 * time.Millisecond); cancel() }()
	var pid int
	start := time.Now()
	out := Run(ctx, RunSpec{Argv: []string{script}, Dir: t.TempDir(), Timeout: time.Minute,
		Parser: &linesParser{keep: 1}, OnStart: func(p int, _ string) error { pid = p; return nil }})
	if out.Reason != "cancelled" {
		t.Fatalf("reason %q", out.Reason)
	}
	if time.Since(start) > terminationGrace+3*time.Second {
		t.Error("cancel took too long")
	}
	for deadline := time.Now().Add(2 * time.Second); processGroupAlive(pid) && time.Now().Before(deadline); {
		time.Sleep(20 * time.Millisecond)
	}
	if processGroupAlive(pid) {
		t.Fatalf("group %d alive", pid)
	}
}

func TestRunExitAndOutput(t *testing.T) {
	script := writeScript(t, "cat\necho err >&2\nprintf 'a\\nb\\n'\nexit 3\n")
	outFile := filepath.Join(t.TempDir(), "out.log")
	out := Run(context.Background(), RunSpec{Argv: []string{script}, Dir: t.TempDir(), Prompt: "PROMPT\n",
		Timeout: 5 * time.Second, Parser: &linesParser{keep: 10}, OutputFile: outFile})
	if out.Reason != "exited" || out.ExitCode != 3 {
		t.Fatalf("outcome %+v", out)
	}
	if out.Result.Text != "PROMPT\na\nb" {
		t.Errorf("result %q", out.Result.Text)
	}
	if strings.TrimSpace(out.StderrTail) != "err" {
		t.Errorf("stderr tail %q", out.StderrTail)
	}
	raw, err := os.ReadFile(outFile)
	if err != nil || !strings.Contains(string(raw), "[stderr] err") || !strings.Contains(string(raw), "PROMPT") {
		t.Errorf("raw output %q %v", raw, err)
	}
	if info, _ := os.Stat(outFile); info.Mode().Perm() != 0o600 {
		t.Errorf("output file mode %o", info.Mode().Perm())
	}
}

func TestKillProcessGroupRefusesChangedIdentity(t *testing.T) {
	if err := killProcessGroup(os.Getpid(), "not-the-real-identity", time.Millisecond); err == nil {
		t.Fatal("killed with wrong identity")
	}
	// Dead pid is not an error.
	if err := killProcessGroup(1<<22-1, "x", time.Millisecond); err != nil && !strings.Contains(err.Error(), "verify") {
		t.Errorf("unexpected: %v", err)
	}
	_ = syscall.Getpgrp()
}
