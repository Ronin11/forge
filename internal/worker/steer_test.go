package worker

import (
	"context"
	"encoding/json"
	"log/slog"
	"os"
	"strings"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/logging"
)

// steerLine is the wire shape WriteUser and StreamInput produce.
type steerLine struct {
	Type    string `json:"type"`
	Message struct {
		Role    string `json:"role"`
		Content []struct {
			Type string `json:"type"`
			Text string `json:"text"`
		} `json:"content"`
	} `json:"message"`
}

func decodeSteerLine(t *testing.T, raw string) steerLine {
	t.Helper()
	var l steerLine
	if err := json.Unmarshal([]byte(raw), &l); err != nil {
		t.Fatalf("line %q: %v", raw, err)
	}
	if l.Type != "user" || l.Message.Role != "user" || len(l.Message.Content) != 1 || l.Message.Content[0].Type != "text" {
		t.Fatalf("line %q is not a stream-json user message", raw)
	}
	return l
}

// The steer capability turns on stream-json input on the command line; an
// executor without it keeps the plain-prompt contract.
func TestExecutorSteerFlag(t *testing.T) {
	cfg := ExecutorConfig{Command: []string{"claude", "--model", "{{model}}"}, Output: "claude-stream-json", Capabilities: []string{CapSteer}}
	e, err := NewTemplateExecutor("claude-code", cfg)
	if err != nil {
		t.Fatal(err)
	}
	cmd, err := e.Command(context.Background(), LaunchRequest{Model: "m", Worktree: t.TempDir()})
	if err != nil {
		t.Fatal(err)
	}
	if got := strings.Join(cmd.Args, " "); !strings.Contains(got, "--input-format stream-json") {
		t.Errorf("steer executor args = %q, want --input-format stream-json", got)
	}
	plain, err := NewTemplateExecutor("plain", ExecutorConfig{Command: cfg.Command, Output: cfg.Output})
	if err != nil {
		t.Fatal(err)
	}
	cmd, err = plain.Command(context.Background(), LaunchRequest{Model: "m", Worktree: t.TempDir()})
	if err != nil {
		t.Fatal(err)
	}
	if got := strings.Join(cmd.Args, " "); strings.Contains(got, "--input-format") {
		t.Errorf("plain executor args = %q, want no --input-format", got)
	}
}

// pollFor is the deadline helper of STYLE.md §5: no sleep-as-synchronisation.
func pollFor(t *testing.T, d time.Duration, what string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(d)
	for time.Now().Before(deadline) {
		if cond() {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", what)
}

// Under StreamInput the prompt arrives as one stream-json user line, stdin
// stays open, and a later WriteUser lands as a second line — proven by a
// child that must read two lines to exit 0.
func TestLaunchStreamInputKeepsStdinOpen(t *testing.T) {
	var stdout lineSink
	p, err := Launch(context.Background(), LaunchSpec{
		Cmd: shell(`read a; echo "$a"; read b; echo "$b"`), Prompt: "go", StreamInput: true,
		Timeout: 10 * time.Second, OnStdout: stdout.add,
	})
	if err != nil {
		t.Fatal(err)
	}
	pollFor(t, 5*time.Second, "the echoed prompt line", func() bool { return len(stdout.get()) >= 1 })
	if got := decodeSteerLine(t, stdout.get()[0]); got.Message.Content[0].Text != "go" {
		t.Errorf("prompt line text = %q, want go", got.Message.Content[0].Text)
	}
	if err := p.WriteUser("north"); err != nil {
		t.Fatal(err)
	}
	st := p.Wait()
	if st.Code != 0 {
		t.Fatalf("exit = %+v (stdin closed early?)", st)
	}
	lines := stdout.get()
	if len(lines) != 2 {
		t.Fatalf("stdout = %v", lines)
	}
	if got := decodeSteerLine(t, lines[1]); got.Message.Content[0].Text != "north" {
		t.Errorf("steer line text = %q, want north", got.Message.Content[0].Text)
	}
	if err := p.WriteUser("late"); err == nil {
		t.Error("WriteUser after exit should refuse")
	}
}

// A plain launch refuses WriteUser: stdin was closed after the prompt.
func TestWriteUserRefusedWithoutStreamInput(t *testing.T) {
	p, err := Launch(context.Background(), LaunchSpec{Cmd: shell(`cat`), Prompt: "x", Timeout: 10 * time.Second})
	if err != nil {
		t.Fatal(err)
	}
	if err := p.WriteUser("nope"); err == nil {
		t.Error("WriteUser without StreamInput should refuse")
	}
	if st := p.Wait(); st.Code != 0 {
		t.Errorf("exit = %+v", st)
	}
}

// Heartbeat-delivered steers reach the live process's stdin; without a
// steerable process they surface as steer.dropped lifecycle events.
func TestAttemptDeliversAndDropsSteers(t *testing.T) {
	d := &fakeDaemon{}
	h := logging.New(os.Stderr, logging.Options{Levels: logging.Levels{Default: slog.LevelWarn}, Format: logging.FormatText}, nil)
	a := &attempt{ctx: context.Background(), log: h.For("worker.attempt"), emitter: NewEmitter(model.NewID(), d, h.For("worker.attempt"), time.Now, 0, 0)}

	// No process yet: dropped, visibly.
	a.applyHeartbeat(&protocol.HeartbeatResponse{Steer: []string{"too early"}})

	var stdout lineSink
	p, err := Launch(context.Background(), LaunchSpec{
		Cmd: shell(`read a; read b; echo ok`), Prompt: "start", StreamInput: true,
		Timeout: 10 * time.Second, OnStdout: stdout.add,
	})
	if err != nil {
		t.Fatal(err)
	}
	a.mu.Lock()
	a.process, a.steerable = p, true
	a.mu.Unlock()
	a.applyHeartbeat(&protocol.HeartbeatResponse{Steer: []string{"go west"}})
	if st := p.Wait(); st.Code != 0 {
		t.Fatalf("exit = %+v", st)
	}
	a.emitter.Flush(context.Background())

	d.mu.Lock()
	defer d.mu.Unlock()
	var dropped, delivered bool
	for _, e := range d.events {
		if e.Kind == protocol.KindLifecycle && e.Message == "steer.dropped" {
			dropped = true
		}
		if e.Kind == protocol.KindLifecycle && e.Message == "steer.delivered" {
			delivered = true
		}
	}
	if !dropped || !delivered {
		t.Errorf("dropped=%v delivered=%v events=%v", dropped, delivered, len(d.events))
	}
}
