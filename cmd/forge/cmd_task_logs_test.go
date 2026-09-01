package main

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/core/daemon"
	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
	"forge/internal/web"
)

func TestReadSSE(t *testing.T) {
	raw := "event: journal\nid: 3\ndata: {\"a\":1}\n\nretry: 2000\n\nevent: end\ndata: {\"state\":\"succeeded\"}\n\n"
	br := bufio.NewReader(strings.NewReader(raw))
	ev, err := readSSE(br)
	if err != nil || ev.event != "journal" || ev.id != "3" || ev.data != `{"a":1}` {
		t.Errorf("first event = %+v, %v", ev, err)
	}
	ev, err = readSSE(br)
	if err != nil || !ev.retry || ev.event != "" {
		t.Errorf("retry event = %+v, %v", ev, err)
	}
	ev, err = readSSE(br)
	if err != nil || ev.event != "end" || ev.data != `{"state":"succeeded"}` {
		t.Errorf("end event = %+v, %v", ev, err)
	}
	if _, err = readSSE(br); !errors.Is(err, io.EOF) {
		t.Errorf("after last event: %v, want EOF", err)
	}
}

func TestFormatStreamLines(t *testing.T) {
	ts := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	j := store.JournalEntry{ID: 7, Time: ts, Kind: "target.transition", EntityType: "target", EntityID: "0123456789abcdef0123456789abcdef", Payload: json.RawMessage(`{"from":"claimed","to":"running","reason":""}`)}
	got := formatJournalLine(j)
	for _, want := range []string{"target.transition", "target 01234567", "claimed→running"} {
		if !strings.Contains(got, want) {
			t.Errorf("journal line %q lacks %q", got, want)
		}
	}
	e := attemptStreamEvent{AttemptID: "fedcba9876543210fedcba9876543210", StoredEvent: store.StoredEvent{Source: "worker", Event: protocol.Event{Seq: 1, Time: ts, Kind: "stdout", Message: "hello\n"}}}
	got = formatEventLine(e)
	for _, want := range []string{"stdout", "[fedcba98]", "hello"} {
		if !strings.Contains(got, want) {
			t.Errorf("event line %q lacks %q", got, want)
		}
	}
	if strings.HasSuffix(got, "\n") {
		t.Errorf("event line keeps its trailing newline: %q", got)
	}
}

func TestTaskLogsUsage(t *testing.T) {
	code, stdout, stderr := run([]string{"task", "logs"}, nil)
	if code != 2 || stdout != "" || !strings.Contains(stderr, "usage: forge task logs") {
		t.Errorf("task logs without ID = %d %q %q", code, stdout, stderr)
	}
}

const logsTestWorker = "0123456789abcdef0123456789abcdef"

// streamHome boots a real control plane on the home's unix socket so the CLI
// commands under test speak to it exactly as they would to a daemon.
func streamHome(t *testing.T) string {
	t.Helper()
	home := t.TempDir()
	st, err := store.Open(context.Background(), filepath.Join(home, daemon.DBFile), store.Options{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	})
	if err := st.Write(context.Background(), func(tx *store.Tx) error { return tx.EnsureProject(context.Background(), "default") }); err != nil {
		t.Fatal(err)
	}
	srv, err := web.NewServer(web.ServerOptions{Store: st, Version: version, StreamInterval: 20 * time.Millisecond})
	if err != nil {
		t.Fatal(err)
	}
	unixL, err := daemon.ListenSocket(home)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- srv.Serve(ctx, unixL, nil) }()
	t.Cleanup(func() {
		cancel()
		if err := <-done; err != nil {
			t.Error(err)
		}
	})
	return home
}

// apiDo is one JSON call over the test home's socket.
func apiDo(t *testing.T, home, method, path string, in, out any) int {
	t.Helper()
	client := &http.Client{Transport: &http.Transport{DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
		var d net.Dialer
		return d.DialContext(ctx, "unix", filepath.Join(home, daemon.SocketFile))
	}}, Timeout: 10 * time.Second}
	var body io.Reader
	if in != nil {
		b, err := json.Marshal(in)
		if err != nil {
			t.Fatal(err)
		}
		body = bytes.NewReader(b)
	}
	req, err := http.NewRequest(method, "http://forge"+path, body)
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	raw, err := io.ReadAll(resp.Body)
	if cerr := resp.Body.Close(); cerr != nil {
		t.Error(cerr)
	}
	if err != nil {
		t.Fatal(err)
	}
	// A 204 (an empty claim) has no body to decode; the claim poll loop below
	// treats the zero value as "nothing yet" and keeps polling.
	if out != nil && resp.StatusCode >= 200 && resp.StatusCode < 300 && len(raw) > 0 {
		if err := json.Unmarshal(raw, out); err != nil {
			t.Fatalf("%s %s: decode %q: %v", method, path, raw, err)
		}
	}
	if resp.StatusCode >= 400 {
		t.Fatalf("%s %s = %d %s", method, path, resp.StatusCode, raw)
	}
	return resp.StatusCode
}

// runHome dispatches one CLI command against the test home.
func runHome(home string, args []string) (code int, stdout, stderr string) {
	var out, errOut strings.Builder
	c := &cmdContext{stdout: &out, stderr: &errOut, getenv: func(string) string { return "" }, forgeHome: home, userHome: home, now: time.Now}
	code = dispatch(context.Background(), commands(), c, args)
	return code, out.String(), errOut.String()
}

func registerLogsWorker(t *testing.T, home string) {
	t.Helper()
	apiDo(t, home, http.MethodPost, "/api/v1/worker/register", protocol.RegisterRequest{
		WorkerID: logsTestWorker, Name: "laptop", Version: "test", MaxConcurrent: 2, Executors: []string{"claude-code"},
		Repositories: []protocol.Repository{{Name: "equitizr", Path: "/tmp/equitizr", OriginIdentity: "github.com/x/equitizr", Project: "default"}},
	}, nil)
}

// finishAttempt drives one claimed attempt to succeeded over the API.
func finishAttempt(t *testing.T, home string, claim *protocol.Claim, lease string) {
	t.Helper()
	now := time.Now().UTC()
	for _, st := range []model.State{model.Preparing, model.Running} {
		apiDo(t, home, http.MethodPost, "/api/v1/attempts/"+claim.AttemptID+"/heartbeat",
			protocol.HeartbeatRequest{LeaseToken: lease, Phase: string(st), State: st, PID: 1, PIDStart: 7, SessionID: "sess-1"}, nil)
	}
	apiDo(t, home, http.MethodPost, "/api/v1/attempts/"+claim.AttemptID+"/complete", protocol.CompleteRequest{
		LeaseToken: lease, State: model.Succeeded, NumTurns: 1, Launches: 1,
		Usage: protocol.Usage{InputTokens: 1, OutputTokens: 1}, Git: protocol.GitOutcome{Head: "abc"},
		Verification: protocol.Verification{Level: 1, Passed: true}, Cleanup: protocol.Cleanup{Outcome: "removed", Reason: "clean"},
		StartedAt: now.Add(-time.Minute), FinishedAt: now, SessionID: "sess-1",
	}, nil)
}

func TestTaskLogsAndWaitOverStream(t *testing.T) {
	home := streamHome(t)
	registerLogsWorker(t, home)

	code, stdout, stderr := runHome(home, []string{"task", "add", "--repo", "equitizr", "say", "hello"})
	if code != 0 {
		t.Fatalf("task add = %d %q %q", code, stdout, stderr)
	}
	fields := strings.Fields(stdout)
	if len(fields) < 2 {
		t.Fatalf("task add output %q", stdout)
	}
	shortID := fields[1]

	var claim protocol.Claim
	apiDo(t, home, http.MethodPost, "/api/v1/worker/claim", protocol.ClaimRequest{WorkerID: logsTestWorker, ClaimRequestID: "r1", LeaseToken: "lease-1"}, &claim)
	if claim.AttemptID == "" {
		t.Fatal("nothing claimed")
	}
	apiDo(t, home, http.MethodPost, "/api/v1/attempts/"+claim.AttemptID+"/events", protocol.EventBatch{
		Source: protocol.SourceWorker,
		Events: []protocol.Event{{Seq: 0, Time: time.Now().UTC(), Kind: protocol.KindLifecycle, Message: "hello from agent"}},
	}, nil)

	// Without -f: everything so far, then exit, while the task still runs.
	code, stdout, stderr = runHome(home, []string{"task", "logs", shortID})
	if code != 0 {
		t.Fatalf("task logs = %d %q %q", code, stdout, stderr)
	}
	for _, want := range []string{"hello from agent", "work.created"} {
		if !strings.Contains(stdout, want) {
			t.Errorf("task logs output lacks %q:\n%s", want, stdout)
		}
	}

	finishAttempt(t, home, &claim, "lease-1")

	// With -f on a finished task: the stream replays and ends by itself.
	code, stdout, stderr = runHome(home, []string{"task", "logs", "-f", shortID})
	if code != 0 {
		t.Fatalf("task logs -f = %d %q %q", code, stdout, stderr)
	}
	if !strings.Contains(stdout, "hello from agent") {
		t.Errorf("task logs -f output lacks the event:\n%s", stdout)
	}
	if !strings.Contains(stderr, "succeeded") {
		t.Errorf("task logs -f stderr lacks the final state: %q", stderr)
	}

	// --wait consumes the stream: a background worker finishes the task.
	go func() {
		deadline := time.Now().Add(10 * time.Second)
		for time.Now().Before(deadline) {
			var c protocol.Claim
			apiDo(t, home, http.MethodPost, "/api/v1/worker/claim", protocol.ClaimRequest{WorkerID: logsTestWorker, ClaimRequestID: "r2", LeaseToken: "lease-2"}, &c)
			if c.AttemptID != "" {
				finishAttempt(t, home, &c, "lease-2")
				return
			}
			time.Sleep(50 * time.Millisecond)
		}
		t.Error("nothing claimable for the --wait task")
	}()
	code, stdout, stderr = runHome(home, []string{"task", "add", "--repo", "equitizr", "--wait", "again"})
	if code != 0 {
		t.Fatalf("task add --wait = %d %q %q", code, stdout, stderr)
	}
	if !strings.Contains(stderr, "succeeded") {
		t.Errorf("task add --wait stderr lacks the final state: %q", stderr)
	}
}
