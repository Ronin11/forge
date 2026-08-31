package main

import (
	"context"
	"log/slog"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func writeRunRepo(t *testing.T, start string) string {
	t.Helper()
	dir := t.TempDir()
	if err := os.MkdirAll(filepath.Join(dir, ".forge"), 0o755); err != nil {
		t.Fatal(err)
	}
	cfg := "[run]\nstart = [\"bash\", \"-c\", \"" + start + "\"]\nport_env = \"PORT\"\nready_log = \"READY\"\nready_timeout = 5\n"
	if err := os.WriteFile(filepath.Join(dir, ".forge", "config.toml"), []byte(cfg), 0o644); err != nil {
		t.Fatal(err)
	}
	return dir
}

func TestRunSupervisorStartStop(t *testing.T) {
	dir := writeRunRepo(t, "echo READY; exec sleep 30")
	sup := newRunSupervisor(t.TempDir(), controlplaneRunConfig{portMin: 3400, portMax: 3410}, nil, slog.New(slog.DiscardHandler), time.Now)
	ctx := context.Background()

	st, err := sup.StartApp(ctx, "t", dir)
	if err != nil {
		t.Fatalf("start: %v", err)
	}
	if !st.Configured || st.Port < 3400 || st.Port > 3410 {
		t.Fatalf("start status = %+v", st)
	}
	// It should reach running once the READY line lands.
	deadline := time.Now().Add(6 * time.Second)
	for {
		s, serr := sup.AppStatus(ctx, "t", dir)
		if serr != nil {
			t.Fatal(serr)
		}
		if s.State == "running" {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("never reached running: %+v", s)
		}
		time.Sleep(100 * time.Millisecond)
	}
	// Starting again while running is refused.
	if _, err := sup.StartApp(ctx, "t", dir); err == nil {
		t.Fatal("second start should be refused")
	}
	// Stop returns stopped and the child is gone.
	pid := sup.procs["t"].cmd.Process.Pid
	if _, err := sup.StopApp(ctx, "t", dir); err != nil {
		t.Fatalf("stop: %v", err)
	}
	time.Sleep(500 * time.Millisecond)
	if err := syscallKill0(pid); err == nil {
		t.Errorf("process %d still alive after stop", pid)
	}
	s, serr := sup.AppStatus(ctx, "t", dir)
	if serr != nil {
		t.Fatal(serr)
	}
	if s.State != "stopped" {
		t.Errorf("state after stop = %q", s.State)
	}
}

func TestRunSupervisorUnconfigured(t *testing.T) {
	dir := t.TempDir() // no .forge/config.toml
	sup := newRunSupervisor(t.TempDir(), controlplaneRunConfig{portMin: 3500, portMax: 3510}, nil, slog.New(slog.DiscardHandler), time.Now)
	st, err := sup.AppStatus(context.Background(), "t", dir)
	if err != nil || st.Configured || st.State != "stopped" {
		t.Fatalf("unconfigured status = %+v err=%v", st, err)
	}
	if _, err := sup.StartApp(context.Background(), "t", dir); err == nil {
		t.Fatal("start with no [run] should error")
	}
}

func TestAppEnvInjectsPort(t *testing.T) {
	env := appEnv(runFixture(), 3456)
	found := false
	for _, e := range env {
		if e == "PORT=3456" {
			found = true
		}
	}
	if !found {
		t.Error("PORT not injected")
	}
	// port 0 must not inject.
	for _, e := range appEnv(runFixture(), 0) {
		if len(e) >= 5 && e[:5] == "PORT=" {
			t.Error("PORT injected for port 0")
		}
	}
}
