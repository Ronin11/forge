package main

import (
	"bufio"
	"context"
	"forge/internal/core/daemon"
	"io"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

// envValues collects every value a key has in an environment slice.
func envValues(env []string, key string) []string {
	var out []string
	for _, kv := range env {
		if v, ok := strings.CutPrefix(kv, key+"="); ok {
			out = append(out, v)
		}
	}
	return out
}

func TestRestartExecSpec(t *testing.T) {
	base := []string{
		"PATH=/usr/bin",
		"INVOCATION_ID=abc", // systemd identity must survive the exec
		"FORGE_HOME=/old",
		"FORGE_SOCK_FD=9", // stale numbers from a previous restart
		"FORGE_HTTP_FD=8",
		"FORGE_RESTARTED=1",
		"FORGE_LOG_LEVEL=info",
	}
	logEnv := []string{"FORGE_LOG_LEVEL=debug", "FORGE_LOG_FORMAT=json"}
	argv, env := restartExecSpec("/opt/forge", "/home/x/.forge", base, logEnv, 3, 5, 6)

	wantArgv := []string{"/opt/forge", "daemon", "start", "--foreground", "--lock-fd", "3"}
	if len(argv) != len(wantArgv) {
		t.Fatalf("argv = %v, want %v", argv, wantArgv)
	}
	for i := range wantArgv {
		if argv[i] != wantArgv[i] {
			t.Fatalf("argv = %v, want %v", argv, wantArgv)
		}
	}
	singles := map[string]string{
		"PATH":              "/usr/bin",
		"INVOCATION_ID":     "abc",
		"FORGE_HOME":        "/home/x/.forge",
		daemon.EnvSockFD:    "5",
		daemon.EnvHTTPFD:    "6",
		daemon.EnvRestarted: "1",
		"FORGE_LOG_LEVEL":   "debug",
		"FORGE_LOG_FORMAT":  "json",
	}
	for key, want := range singles {
		got := envValues(env, key)
		if len(got) != 1 || got[0] != want {
			t.Errorf("%s = %v, want exactly [%s]\nenv: %v", key, got, want, env)
		}
	}
}

func TestDaemonLogsFollow(t *testing.T) {
	home := t.TempDir()
	if err := os.MkdirAll(filepath.Join(home, "logs"), 0o700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(home, "logs", "daemon.log")
	if err := os.WriteFile(path, []byte("one\ntwo\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	pr, pw := io.Pipe()
	var errOut strings.Builder
	c := &cmdContext{stdout: pw, stderr: &errOut, getenv: func(string) string { return "" }, forgeHome: home, userHome: home, now: time.Now}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan int, 1)
	go func() {
		code := runDaemonLogs(ctx, c, []string{"-f"})
		if err := pw.Close(); err != nil {
			t.Error(err)
		}
		done <- code
	}()
	br := bufio.NewReader(pr)
	readLine := func() string {
		t.Helper()
		line, err := br.ReadString('\n')
		if err != nil {
			t.Fatalf("read tail output: %v (stderr %q)", err, errOut.String())
		}
		return strings.TrimSuffix(line, "\n")
	}
	if got := readLine(); got != "one" {
		t.Errorf("first line = %q", got)
	}
	if got := readLine(); got != "two" {
		t.Errorf("second line = %q", got)
	}
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND, 0o600)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := f.WriteString("three\n"); err != nil {
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
	if got := readLine(); got != "three" { // the poll picks it up within 500 ms
		t.Errorf("followed line = %q", got)
	}
	cancel()
	if code := <-done; code != 0 {
		t.Errorf("daemon logs -f exit = %d (stderr %q)", code, errOut.String())
	}
}

// The lock fd must be CLOEXEC in a running daemon (children never inherit
// it — M6 smoke 6 wedged auto-start on a worker-inherited lock) and cleared
// only for the exec.
func TestSetCloexec(t *testing.T) {
	f, err := os.CreateTemp(t.TempDir(), "fd")
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := f.Close(); err != nil {
			t.Fatal(err)
		}
	}()
	get := func() uintptr {
		flags, _, e := syscall.Syscall(syscall.SYS_FCNTL, f.Fd(), syscall.F_GETFD, 0)
		if e != 0 {
			t.Fatalf("F_GETFD: %v", e)
		}
		return flags
	}
	if err := setCloexec(f.Fd(), true); err != nil {
		t.Fatal(err)
	}
	if get()&syscall.FD_CLOEXEC == 0 {
		t.Error("cloexec not set")
	}
	if err := setCloexec(f.Fd(), false); err != nil {
		t.Fatal(err)
	}
	if get()&syscall.FD_CLOEXEC != 0 {
		t.Error("cloexec not cleared")
	}
}
