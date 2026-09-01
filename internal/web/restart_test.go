package web

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"syscall"
	"testing"
	"time"

	"forge/internal/core/daemon"
	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

// fakeExe writes an executable file the drain validation accepts.
func fakeExe(t *testing.T) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "forge")
	if err := os.WriteFile(path, []byte("#!/bin/sh\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestDrainRejectsBadExec(t *testing.T) {
	h := newHarness(t, transportUnix)
	dir := t.TempDir()
	plain := filepath.Join(dir, "notes.txt")
	if err := os.WriteFile(plain, []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
	cases := []struct {
		name string
		exec string
	}{
		{"relative path", "forge"},
		{"missing file", filepath.Join(dir, "gone")},
		{"directory", dir},
		{"not executable", plain},
		{"no exec support in this server", fakeExe(t)}, // harness has no ExecRestart
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			status, body := h.do(http.MethodPost, "/api/v1/daemon/drain", drainBody{Exec: c.exec}, nil, "")
			if status != http.StatusBadRequest {
				t.Errorf("drain exec=%q = %d %s", c.exec, status, body)
			}
			if h.srv.Draining() {
				t.Error("a refused drain still set the flag")
			}
		})
	}
}

// TestDrainedServerStillAcceptsWorkerPaths is the §1.4 regression: while
// draining, claims 503 but the heartbeat, event, and completion paths of a
// running attempt keep answering 200.
func TestDrainedServerStillAcceptsWorkerPaths(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	h.run("inventory")
	c := h.mustClaim("r1")
	h.srv.SetDraining(true)
	if status, _ := h.claim(testWorkerID, "r2"); status != http.StatusServiceUnavailable {
		t.Errorf("claim while draining = %d", status)
	}
	h.heartbeat(c, model.Preparing, 0)
	batch := protocol.EventBatch{Source: protocol.SourceWorker, Events: []protocol.Event{{Seq: 0, Time: h.clock.Now(), Kind: protocol.KindLifecycle, Message: "still flowing"}}}
	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/events", batch, nil, http.StatusOK)
	h.heartbeat(c, model.Running, 1)
	if done := h.complete(c, completeRequest(model.Succeeded, h.clock.Now())); done.State != model.Succeeded {
		t.Errorf("complete while draining = %+v", done)
	}
}

func TestDrainExecWaitsForInflight(t *testing.T) {
	h := newHarness(t, transportUnix)
	called := make(chan string, 1)
	h.srv.execRestart = func(path string) error {
		called <- path
		return nil
	}
	exe := fakeExe(t)
	h.srv.inflight.Add(1) // a pretend in-flight request
	var state map[string]string
	h.call(http.MethodPost, "/api/v1/daemon/drain", drainBody{Exec: exe, TimeoutSeconds: 30}, &state, http.StatusOK)
	if state["state"] != "draining" || !h.srv.Draining() {
		t.Fatalf("drain = %v", state)
	}
	select {
	case p := <-called:
		t.Fatalf("exec of %s before in-flight requests finished", p)
	case <-time.After(150 * time.Millisecond):
	}
	h.srv.inflight.Add(-1)
	select {
	case p := <-called:
		if p != exe {
			t.Errorf("exec path = %q, want %q", p, exe)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("exec never ran after in-flight requests finished")
	}
	var journal []store.JournalEntry
	h.call(http.MethodGet, "/api/v1/journal?since=0&limit=100", nil, &journal, http.StatusOK)
	if !hasKind(journal, "daemon.draining") {
		t.Errorf("journal lacks daemon.draining: %d entries", len(journal))
	}
}

// dupFD duplicates a listener's descriptor into an fd nothing else owns, the
// shape daemon.ListenerFromFD sees after an exec.
func dupFD(t *testing.T, f *os.File) uintptr {
	t.Helper()
	fd, err := syscall.Dup(int(f.Fd()))
	if err != nil {
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
	return uintptr(fd)
}

func TestListenerAdoptionTCP(t *testing.T) {
	h := newHarness(t, "")
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	tl, ok := l.(*net.TCPListener)
	if !ok {
		t.Fatalf("listener is %T", l)
	}
	f, err := tl.File()
	if err != nil {
		t.Fatal(err)
	}
	adopted, err := daemon.ListenerFromFD(dupFD(t, f), "http")
	if err != nil {
		t.Fatal(err)
	}
	if err := l.Close(); err != nil { // the old image's listener; the adopted one lives on
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- h.srv.Serve(ctx, nil, adopted) }()
	resp, err := http.Get(fmt.Sprintf("http://%s/api/v1/handshake", adopted.Addr()))
	if err != nil {
		t.Fatal(err)
	}
	if err := resp.Body.Close(); err != nil {
		t.Error(err)
	}
	if resp.StatusCode != http.StatusOK {
		t.Errorf("handshake over adopted listener = %d", resp.StatusCode)
	}
	cancel()
	if err := <-done; err != nil {
		t.Error(err)
	}
}

func TestListenerAdoptionUnix(t *testing.T) {
	h := newHarness(t, "")
	home := t.TempDir()
	l, err := daemon.ListenSocket(home)
	if err != nil {
		t.Fatal(err)
	}
	ul, ok := l.(*net.UnixListener)
	if !ok {
		t.Fatalf("listener is %T", l)
	}
	ul.SetUnlinkOnClose(false) // as after exec: the old image never unlinks
	f, err := ul.File()
	if err != nil {
		t.Fatal(err)
	}
	adopted, err := daemon.ListenerFromFD(dupFD(t, f), daemon.SocketFile)
	if err != nil {
		t.Fatal(err)
	}
	if err := l.Close(); err != nil {
		t.Fatal(err)
	}
	sock := filepath.Join(home, daemon.SocketFile)
	if _, err := os.Stat(sock); err != nil {
		t.Fatalf("socket file gone after adoption: %v", err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- h.srv.Serve(ctx, adopted, nil) }()
	client := &http.Client{Transport: &http.Transport{DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
		var d net.Dialer
		return d.DialContext(ctx, "unix", sock)
	}}, Timeout: 5 * time.Second}
	resp, err := client.Get("http://forge/api/v1/handshake")
	if err != nil {
		t.Fatal(err)
	}
	if err := resp.Body.Close(); err != nil {
		t.Error(err)
	}
	if resp.StatusCode != http.StatusOK {
		t.Errorf("handshake over adopted socket = %d", resp.StatusCode)
	}
	cancel()
	if err := <-done; err != nil {
		t.Error(err)
	}
}
