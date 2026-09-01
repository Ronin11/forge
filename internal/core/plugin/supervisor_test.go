package plugin

import (
	"context"
	"strings"
	"sync"
	"testing"
	"time"
)

// waitFor polls cond until it holds or the deadline passes (STYLE.md §5: no
// sleeping for synchronisation, polling with a deadline).
func waitFor(t *testing.T, d time.Duration, what string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(d)
	for time.Now().Before(deadline) {
		if cond() {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", what)
}

// syncBuf is a goroutine-safe log sink.
type syncBuf struct {
	mu sync.Mutex // guards b
	b  strings.Builder
}

func (s *syncBuf) Write(p []byte) (int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.b.Write(p)
}

func (s *syncBuf) String() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.b.String()
}

func shManifest(t *testing.T, name, restart, script string) *Manifest {
	t.Helper()
	return &Manifest{
		Name: name, Version: "0.0.1", Command: []string{"/bin/sh", "-c", script},
		Capabilities: []string{CapEvents}, Restart: restart, Dir: t.TempDir(),
	}
}

func testSupervisor() *Supervisor {
	return NewSupervisor(SupervisorOptions{
		Backoff:   Backoff{Initial: 20 * time.Millisecond, Max: 80 * time.Millisecond, ResetAfter: time.Hour},
		KillGrace: 200 * time.Millisecond,
	})
}

func health(s *Supervisor, name string) (PluginHealth, bool) {
	for _, h := range s.Health() {
		if h.Name == name {
			return h, true
		}
	}
	return PluginHealth{}, false
}

func TestSupervisorNeverRestarts(t *testing.T) {
	t.Parallel()
	s := testSupervisor()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	sink := &syncBuf{}
	spec := Spec{Manifest: shManifest(t, "oneshot", "never", "echo from-stdout; echo from-stderr 1>&2; exit 3"), LogSink: sink}
	if err := s.Start(ctx, spec); err != nil {
		t.Fatal(err)
	}
	waitFor(t, 5*time.Second, "exit recorded", func() bool {
		h, ok := health(s, "oneshot")
		return ok && !h.Running && h.LastExit != ""
	})
	h, _ := health(s, "oneshot")
	if h.Restarts != 0 {
		t.Errorf("restarts = %d, want 0 (restart=never)", h.Restarts)
	}
	if !strings.Contains(h.LastExit, "exit status 3") {
		t.Errorf("last exit = %q, want exit status 3", h.LastExit)
	}
	// Both streams reach the sink line-by-line.
	waitFor(t, 5*time.Second, "log lines captured", func() bool {
		out := sink.String()
		return strings.Contains(out, "from-stdout") && strings.Contains(out, "from-stderr")
	})
	// Still down after a few backoff periods: never means never.
	deadline := time.Now().Add(150 * time.Millisecond)
	for time.Now().Before(deadline) {
		if h, _ := health(s, "oneshot"); h.Restarts != 0 || h.Running {
			t.Fatalf("restart=never plugin restarted: %+v", h)
		}
		time.Sleep(10 * time.Millisecond)
	}
	cancel()
	s.Wait()
}

func TestSupervisorOnFailureRestartsWithBackoff(t *testing.T) {
	t.Parallel()
	s := testSupervisor()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	spec := Spec{Manifest: shManifest(t, "crasher", "on-failure", "exit 1"), LogSink: &syncBuf{}}
	start := time.Now()
	if err := s.Start(ctx, spec); err != nil {
		t.Fatal(err)
	}
	waitFor(t, 10*time.Second, "three restarts", func() bool {
		h, ok := health(s, "crasher")
		return ok && h.Restarts >= 3
	})
	// Backoff 20+40+80 ms means three restarts cannot arrive faster than the
	// schedule's sum (loose lower bound: the first two delays).
	if elapsed := time.Since(start); elapsed < 60*time.Millisecond {
		t.Errorf("three restarts in %v: backoff not applied", elapsed)
	}
	h, _ := health(s, "crasher")
	if !strings.Contains(h.LastExit, "exit status 1") {
		t.Errorf("last exit = %q, want exit status 1", h.LastExit)
	}
	cancel()
	s.Wait()
}

func TestSupervisorOnFailureLeavesCleanExitDown(t *testing.T) {
	t.Parallel()
	s := testSupervisor()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	spec := Spec{Manifest: shManifest(t, "clean", "on-failure", "exit 0"), LogSink: &syncBuf{}}
	if err := s.Start(ctx, spec); err != nil {
		t.Fatal(err)
	}
	waitFor(t, 5*time.Second, "clean exit", func() bool {
		h, ok := health(s, "clean")
		return ok && !h.Running && h.LastExit == "exit ok"
	})
	deadline := time.Now().Add(150 * time.Millisecond)
	for time.Now().Before(deadline) {
		if h, _ := health(s, "clean"); h.Restarts != 0 {
			t.Fatalf("clean exit restarted under on-failure: %+v", h)
		}
		time.Sleep(10 * time.Millisecond)
	}
	cancel()
	s.Wait()
}

func TestSupervisorAlwaysRestartsCleanExit(t *testing.T) {
	t.Parallel()
	s := testSupervisor()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	spec := Spec{Manifest: shManifest(t, "loopy", "always", "exit 0"), LogSink: &syncBuf{}}
	if err := s.Start(ctx, spec); err != nil {
		t.Fatal(err)
	}
	waitFor(t, 10*time.Second, "restart after clean exit", func() bool {
		h, ok := health(s, "loopy")
		return ok && h.Restarts >= 1
	})
	cancel()
	s.Wait()
}

func TestSupervisorStopsOnContext(t *testing.T) {
	t.Parallel()
	s := testSupervisor()
	ctx, cancel := context.WithCancel(context.Background())
	spec := Spec{Manifest: shManifest(t, "longrun", "always", "while true; do sleep 1; done"), LogSink: &syncBuf{}}
	if err := s.Start(ctx, spec); err != nil {
		t.Fatal(err)
	}
	waitFor(t, 5*time.Second, "running", func() bool {
		h, ok := health(s, "longrun")
		return ok && h.Running && h.PID > 0
	})
	cancel()
	done := make(chan struct{})
	go func() { s.Wait(); close(done) }()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("supervisor did not stop the plugin on ctx done")
	}
}

func TestSupervisorKillsAfterGrace(t *testing.T) {
	t.Parallel()
	s := testSupervisor()
	ctx, cancel := context.WithCancel(context.Background())
	spec := Spec{Manifest: shManifest(t, "stubborn", "always", `trap "" TERM; while true; do sleep 1; done`), LogSink: &syncBuf{}}
	if err := s.Start(ctx, spec); err != nil {
		t.Fatal(err)
	}
	waitFor(t, 5*time.Second, "running", func() bool {
		h, ok := health(s, "stubborn")
		return ok && h.Running
	})
	cancel()
	done := make(chan struct{})
	go func() { s.Wait(); close(done) }()
	select {
	case <-done: // SIGKILL after the 200 ms grace
	case <-time.After(5 * time.Second):
		t.Fatal("SIGKILL after the grace did not end the plugin")
	}
}

func TestSupervisorStopRemovesHealth(t *testing.T) {
	t.Parallel()
	s := testSupervisor()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	spec := Spec{Manifest: shManifest(t, "gone", "always", "while true; do sleep 1; done"), LogSink: &syncBuf{}}
	if err := s.Start(ctx, spec); err != nil {
		t.Fatal(err)
	}
	waitFor(t, 5*time.Second, "running", func() bool {
		h, ok := health(s, "gone")
		return ok && h.Running
	})
	s.Stop("gone")
	if _, ok := health(s, "gone"); ok {
		t.Error("health row survived Stop")
	}
	s.Wait()
}

func TestBackoffNext(t *testing.T) {
	t.Parallel()
	b := Backoff{Initial: time.Second, Max: time.Minute, ResetAfter: 5 * time.Minute}.withDefaults()
	cases := []struct {
		prev, uptime, want time.Duration
	}{
		{time.Second, 0, 2 * time.Second},
		{2 * time.Second, time.Second, 4 * time.Second},
		{40 * time.Second, 0, time.Minute},          // capped
		{time.Minute, 0, time.Minute},               // stays capped
		{time.Minute, 5 * time.Minute, time.Second}, // healthy run resets
	}
	for _, c := range cases {
		if got := b.next(c.prev, c.uptime); got != c.want {
			t.Errorf("next(%v, %v) = %v, want %v", c.prev, c.uptime, got, c.want)
		}
	}
}

func TestResolveCommand(t *testing.T) {
	t.Parallel()
	cases := []struct{ arg0, dir, want string }{
		{"./bin", "/p/d", "/p/d/bin"},
		{"sub/bin", "/p/d", "/p/d/sub/bin"},
		{"/abs/bin", "/p/d", "/abs/bin"},
		{"python3", "/p/d", "python3"},
	}
	for _, c := range cases {
		if got := resolveCommand(c.arg0, c.dir); got != c.want {
			t.Errorf("resolveCommand(%q, %q) = %q, want %q", c.arg0, c.dir, got, c.want)
		}
	}
}

func TestNewToken(t *testing.T) {
	t.Parallel()
	tok, hash, err := NewToken()
	if err != nil {
		t.Fatal(err)
	}
	if len(tok) != 64 {
		t.Errorf("token length = %d, want 64 hex chars", len(tok))
	}
	if HashToken(tok) != hash {
		t.Error("HashToken(token) does not match the minted hash")
	}
	tok2, _, err := NewToken()
	if err != nil {
		t.Fatal(err)
	}
	if tok == tok2 {
		t.Error("two minted tokens are identical")
	}
}
