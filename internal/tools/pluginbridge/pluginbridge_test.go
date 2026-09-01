package pluginbridge

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"testing"
	"time"

	"forge/internal/tools"
	"forge/internal/tools/mcpserve"
)

// TestPluginToolsBridge speaks one real initialize → tools/list → tools/call
// exchange against mcpserve's own server over pipes — the same
// newline-delimited JSON-RPC a tools plugin serves on its stdio.
func TestPluginToolsBridge(t *testing.T) {
	t.Parallel()
	inR, inW := io.Pipe()   // bridge → plugin stdin
	outR, outW := io.Pipe() // plugin stdout → bridge
	fake := &mcpserve.Server{
		Version: "test",
		Tools: []mcpserve.ToolDef{{
			Name: "ping", Description: "answers pong",
			InputSchema: json.RawMessage(`{"type":"object","additionalProperties":false}`),
		}},
		Call: func(_ context.Context, name string, args json.RawMessage) (json.RawMessage, bool) {
			if name != "ping" {
				return json.RawMessage(`"no such tool"`), true
			}
			if bytes.Contains(args, []byte("fail")) {
				return json.RawMessage(`"boom"`), true
			}
			return json.RawMessage(`{"pong":true}`), false
		},
	}
	serveDone := make(chan error, 1)
	go func() { serveDone <- fake.Serve(context.Background(), inR, outW) }()

	pt := NewPluginTools("echo-tools", slog.New(slog.DiscardHandler))
	pt.Attach(inW, outR)

	reg := tools.NewRegistry()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := RegisterPluginTools(ctx, reg, pt); err != nil {
		t.Fatal(err)
	}
	tool, ok := reg.Get("echo_tools_ping")
	if !ok {
		t.Fatalf("echo_tools_ping not registered; tools: %v", reg.All())
	}
	if tool.Where() != tools.WhereDaemon {
		t.Errorf("where = %q, want daemon", tool.Where())
	}
	out, err := tool.Call(ctx, tools.Request{Input: json.RawMessage(`{}`)})
	if err != nil {
		t.Fatal(err)
	}
	var res struct {
		Pong bool `json:"pong"`
	}
	if err := json.Unmarshal(out, &res); err != nil || !res.Pong {
		t.Errorf("call result = %s (err %v), want {\"pong\":true}", out, err)
	}
	if _, err := tool.Call(ctx, tools.Request{Input: json.RawMessage(`{"mode":"fail"}`)}); err == nil {
		t.Error("isError result did not become a call error")
	}
	// Child gone: the bridge reports the plugin as not running.
	if err := outW.Close(); err != nil {
		t.Fatal(err)
	}
	if err := inR.Close(); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(5 * time.Second)
	for {
		if _, err := tool.Call(ctx, tools.Request{Input: json.RawMessage(`{}`)}); err != nil {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("calls kept succeeding after the plugin exited")
		}
		time.Sleep(5 * time.Millisecond)
	}
	if err := <-serveDone; err != nil {
		t.Logf("fake server exit: %v", err) // pipe teardown; informational
	}
}

func TestPluginToolName(t *testing.T) {
	t.Parallel()
	if got := PluginToolName("echo-tools", "ping"); got != "echo_tools_ping" {
		t.Errorf("PluginToolName = %q, want echo_tools_ping", got)
	}
	if got := PluginToolName("x", "do-thing"); got != "x_do-thing" {
		t.Errorf("PluginToolName = %q (tool names pass through)", got)
	}
}
