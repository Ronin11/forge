package mcpserve

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"forge/internal/core/logging"
	"forge/internal/core/protocol"
)

// rpcReply decodes any response line the transport writes.
type rpcReply struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id"`
	Result  json.RawMessage `json:"result"`
	Error   *rpcError       `json:"error"`
}

func TestServeTransport(t *testing.T) {
	srv := &Server{
		Version: "test-build",
		Tools:   []ToolDef{{Name: "echo", Description: "echoes its arguments", InputSchema: json.RawMessage(`{"type":"object"}`)}},
		Call: func(_ context.Context, name string, args json.RawMessage) (json.RawMessage, bool) {
			if name != "echo" {
				return json.RawMessage("unknown tool " + name), true
			}
			if len(args) == 0 {
				return json.RawMessage("{}"), false
			}
			return args, false
		},
	}
	script := strings.Join([]string{
		`{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}`,
		`{"jsonrpc":"2.0","method":"notifications/initialized"}`,
		`{"jsonrpc":"2.0","id":2,"method":"tools/list"}`,
		`{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"a":1}}}`,
		`{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope","arguments":{}}}`,
		`this is not json`,
		`{"jsonrpc":"2.0","id":5,"method":"bogus/method"}`,
		`{"jsonrpc":"2.0","id":6,"method":"ping"}`,
		`{"jsonrpc":"2.0","id":7,"method":"initialize","params":{}}`,
	}, "\n") + "\n"
	var out bytes.Buffer
	if err := srv.Serve(context.Background(), strings.NewReader(script), &out); err != nil {
		t.Fatalf("Serve: %v", err)
	}
	lines := strings.Split(strings.TrimSpace(out.String()), "\n")
	if len(lines) != 8 {
		t.Fatalf("got %d response lines, want 8:\n%s", len(lines), out.String())
	}
	replies := make([]rpcReply, len(lines))
	for i, line := range lines {
		if err := json.Unmarshal([]byte(line), &replies[i]); err != nil {
			t.Fatalf("response %d is not JSON: %v: %s", i, err, line)
		}
		if replies[i].JSONRPC != "2.0" {
			t.Errorf("response %d: jsonrpc %q, want 2.0", i, replies[i].JSONRPC)
		}
	}

	// initialize echoes the client's protocol version.
	var init initializeResult
	mustUnmarshal(t, replies[0].Result, &init)
	if init.ProtocolVersion != "2025-03-26" {
		t.Errorf("initialize protocolVersion %q, want the client's 2025-03-26", init.ProtocolVersion)
	}
	if init.ServerInfo.Name != "forge" || init.ServerInfo.Version != "test-build" {
		t.Errorf("serverInfo %+v", init.ServerInfo)
	}
	if init.Capabilities.Tools.ListChanged {
		t.Error("listChanged should be false")
	}

	// tools/list carries the tool and its schema under the MCP key.
	var list toolsListResult
	mustUnmarshal(t, replies[1].Result, &list)
	if len(list.Tools) != 1 || list.Tools[0].Name != "echo" || string(list.Tools[0].InputSchema) != `{"type":"object"}` {
		t.Errorf("tools/list: %s", replies[1].Result)
	}

	// A successful call carries the result bytes as text content.
	var call toolCallResult
	mustUnmarshal(t, replies[2].Result, &call)
	if call.IsError || len(call.Content) != 1 || call.Content[0].Type != "text" || call.Content[0].Text != `{"a":1}` {
		t.Errorf("tools/call echo: %s", replies[2].Result)
	}

	// A failed tool is an isError result, never a JSON-RPC error.
	var bad toolCallResult
	mustUnmarshal(t, replies[3].Result, &bad)
	if !bad.IsError || bad.Content[0].Text != "unknown tool nope" {
		t.Errorf("tools/call nope: %s", replies[3].Result)
	}
	if replies[3].Error != nil {
		t.Error("tool failure must not be a JSON-RPC error")
	}

	// Malformed line: -32700 with a null id.
	if replies[4].Error == nil || replies[4].Error.Code != -32700 || string(replies[4].ID) != "null" {
		t.Errorf("parse error response: %s", lines[4])
	}

	// Unknown method with an id: -32601.
	if replies[5].Error == nil || replies[5].Error.Code != -32601 || string(replies[5].ID) != "5" {
		t.Errorf("method-not-found response: %s", lines[5])
	}

	// ping answers an empty object.
	if string(replies[6].Result) != "{}" {
		t.Errorf("ping result %s, want {}", replies[6].Result)
	}

	// initialize without a client version falls back.
	var init2 initializeResult
	mustUnmarshal(t, replies[7].Result, &init2)
	if init2.ProtocolVersion != "2025-06-18" {
		t.Errorf("fallback protocolVersion %q, want 2025-06-18", init2.ProtocolVersion)
	}
}

func mustUnmarshal(t *testing.T, raw json.RawMessage, v any) {
	t.Helper()
	if err := json.Unmarshal(raw, v); err != nil {
		t.Fatalf("decode %s: %v", raw, err)
	}
}

// fakeDaemon implements the daemon's tools contract and records what arrives.
type fakeDaemon struct {
	mux *http.ServeMux
	// mu guards batches and noteInputs.
	mu         sync.Mutex
	batches    []protocol.EventBatch
	noteInputs []string
}

func newFakeDaemon(t *testing.T, attemptID, worktree, baseCommit string) *fakeDaemon {
	t.Helper()
	fd := &fakeDaemon{mux: http.NewServeMux()}
	listing := toolsResponse{
		SchemaVersion: 1,
		Attempt: attemptInfo{
			ID: attemptID, TargetID: strings.Repeat("11", 16), WorkID: strings.Repeat("22", 16),
			WorktreePath: worktree, Branch: "forge/test", BaseBranch: "master",
			BaseCommit: baseCommit, Launches: 0, Mode: "run", Autonomy: "auto", Repository: "demo",
		},
		Tools: []toolListing{
			{Name: "forge_repo_status", Description: "worktree status", Where: "local"},
			{Name: "forge_check", Description: "run declared checks", Where: "local"},
			{Name: "forge_diff_summary", Description: "diff vs base", Where: "local"},
			{Name: "forge_note", Description: "record a note", InputSchema: json.RawMessage(`{"type":"object"}`), Where: "daemon"},
			{Name: "forge_missing", Description: "always 404s", InputSchema: json.RawMessage(`{"type":"object"}`), Where: "daemon"},
		},
	}
	fd.mux.HandleFunc("GET /api/v1/tools", func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Query().Get("attempt_id") != attemptID {
			http.Error(w, `{"error":"unknown attempt"}`, http.StatusNotFound)
			return
		}
		writeJSON(t, w, listing)
	})
	fd.mux.HandleFunc("POST /api/v1/tools/{name}", func(w http.ResponseWriter, r *http.Request) {
		var req toolCallRequest
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil || req.AttemptID != attemptID || req.SchemaVersion != 1 {
			http.Error(w, `{"error":"bad tool request"}`, http.StatusBadRequest)
			return
		}
		switch r.PathValue("name") {
		case "forge_note":
			fd.mu.Lock()
			fd.noteInputs = append(fd.noteInputs, string(req.Input))
			fd.mu.Unlock()
			writeJSON(t, w, toolCallResponse{SchemaVersion: 1, Output: json.RawMessage(`{"noted":true}`)})
		default:
			http.Error(w, `{"error":"no such tool"}`, http.StatusNotFound)
		}
	})
	fd.mux.HandleFunc("POST /api/v1/attempts/{id}/events", func(w http.ResponseWriter, r *http.Request) {
		if r.PathValue("id") != attemptID {
			http.Error(w, `{"error":"unknown attempt"}`, http.StatusNotFound)
			return
		}
		var batch protocol.EventBatch
		if err := json.NewDecoder(r.Body).Decode(&batch); err != nil {
			http.Error(w, `{"error":"bad batch"}`, http.StatusBadRequest)
			return
		}
		fd.mu.Lock()
		fd.batches = append(fd.batches, batch)
		fd.mu.Unlock()
		w.WriteHeader(http.StatusOK)
	})
	return fd
}

func writeJSON(t *testing.T, w http.ResponseWriter, v any) {
	t.Helper()
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(v); err != nil {
		t.Errorf("encode fake response: %v", err)
	}
}

// testRepo builds a real repository: one base commit, one commit ahead of it
// (b.txt and forge.toml), and one untracked file.
func testRepo(t *testing.T) (worktree, baseCommit string) {
	t.Helper()
	cfg := filepath.Join(t.TempDir(), "gitconfig")
	writeFile(t, cfg, "[user]\n\tname = Forge Test\n\temail = test@forge.invalid\n")
	t.Setenv("GIT_CONFIG_GLOBAL", cfg)
	t.Setenv("GIT_CONFIG_SYSTEM", os.DevNull)
	dir := t.TempDir()
	gitRun(t, dir, "init", "-b", "master")
	writeFile(t, filepath.Join(dir, "a.txt"), "base\n")
	gitRun(t, dir, "add", "-A")
	gitRun(t, dir, "commit", "-m", "base")
	base := gitRun(t, dir, "rev-parse", "HEAD")
	writeFile(t, filepath.Join(dir, "b.txt"), "hello\n")
	writeFile(t, filepath.Join(dir, "forge.toml"), "[checks]\nok = [\"true\"]\nbad = [\"false\"]\n")
	gitRun(t, dir, "add", "-A")
	gitRun(t, dir, "commit", "-m", "second")
	writeFile(t, filepath.Join(dir, "untracked.txt"), "loose\n")
	return dir, base
}

func writeFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
}

func gitRun(t *testing.T, dir string, args ...string) string {
	t.Helper()
	cmd := exec.Command("git", args...)
	cmd.Dir = dir
	cmd.Env = append(os.Environ(), "GIT_TERMINAL_PROMPT=0")
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git %v: %v\n%s", args, err, out)
	}
	return strings.TrimSpace(string(out))
}

// spanAttrs is the union of start/end span attributes for assertions.
type spanAttrs struct {
	Tool         string `json:"tool"`
	InputBytes   int    `json:"input_bytes"`
	InputSHA256  string `json:"input_sha256"`
	OutputBytes  int    `json:"output_bytes"`
	OutputSHA256 string `json:"output_sha256"`
	IsError      bool   `json:"is_error"`
	Clock        string `json:"clock"`
}

func sum(b []byte) string {
	s := sha256.Sum256(b)
	return hex.EncodeToString(s[:])
}

func TestBridgeToolsAndSpans(t *testing.T) {
	worktree, base := testRepo(t)
	attemptID := strings.Repeat("abcd1234", 4)
	fd := newFakeDaemon(t, attemptID, worktree, base)
	ts := httptest.NewServer(fd.mux)
	defer ts.Close()
	getenv := func(k string) string {
		switch k {
		case "FORGE_HTTP":
			return ts.URL
		case "FORGE_TOKEN":
			return "test-token"
		}
		return ""
	}
	client, err := NewClient(getenv)
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	ctx := context.Background()
	bridge, err := NewBridge(ctx, client, attemptID, logging.Discard().For("mcp"))
	if err != nil {
		t.Fatalf("NewBridge: %v", err)
	}
	if got := len(bridge.Tools()); got != 5 {
		t.Fatalf("advertised %d tools, want 5", got)
	}
	for _, def := range bridge.Tools() {
		if def.Name == "forge_repo_status" && !strings.Contains(string(def.InputSchema), `"additionalProperties":false`) {
			t.Errorf("local tool without a daemon schema should fall back to the builtin one, got %s", def.InputSchema)
		}
	}
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		bridge.RunSender(ctx)
	}()

	type callRecord struct {
		name    string
		input   []byte
		output  []byte
		isError bool
	}
	var calls []callRecord
	call := func(name, input string) (json.RawMessage, bool) {
		out, isError := bridge.Call(ctx, name, json.RawMessage(input))
		calls = append(calls, callRecord{name: name, input: []byte(input), output: out, isError: isError})
		return out, isError
	}

	// forge_repo_status: one commit ahead, dirty from the untracked file.
	out, isError := call("forge_repo_status", "{}")
	if isError {
		t.Fatalf("forge_repo_status: %s", out)
	}
	var rs repoStatusResult
	mustUnmarshal(t, out, &rs)
	head := gitRun(t, worktree, "rev-parse", "HEAD")
	want := repoStatusResult{SchemaVersion: 1, Repository: "demo", Branch: "forge/test", BaseBranch: "master",
		BaseCommit: base, Head: head, Dirty: true, Ahead: 1, ChangedPaths: []string{"untracked.txt"}}
	if fmt.Sprint(rs) != fmt.Sprint(want) {
		t.Errorf("repo_status:\n got %+v\nwant %+v", rs, want)
	}

	// forge_diff_summary: committed files with counts, untracked flagged.
	out, isError = call("forge_diff_summary", "{}")
	if isError {
		t.Fatalf("forge_diff_summary: %s", out)
	}
	var ds diffSummaryResult
	mustUnmarshal(t, out, &ds)
	if ds.Base != base || ds.Head != head || ds.TotalInsertions != 4 || ds.TotalDeletions != 0 {
		t.Errorf("diff_summary totals: %+v", ds)
	}
	byPath := map[string]diffFile{}
	for _, f := range ds.Files {
		byPath[f.Path] = f
	}
	if f := byPath["b.txt"]; f.Insertions != 1 || f.Deletions != 0 || f.Uncommitted {
		t.Errorf("b.txt entry: %+v", f)
	}
	if f := byPath["forge.toml"]; f.Insertions != 3 || f.Uncommitted {
		t.Errorf("forge.toml entry: %+v", f)
	}
	if f := byPath["untracked.txt"]; f.Insertions != -1 || f.Deletions != -1 || !f.Uncommitted {
		t.Errorf("untracked.txt entry: %+v", f)
	}
	if len(ds.Files) != 3 {
		t.Errorf("diff_summary files: %+v", ds.Files)
	}

	// forge_check without a name runs all declared checks, sorted.
	out, isError = call("forge_check", "{}")
	if isError {
		t.Fatalf("forge_check: %s", out)
	}
	var cr checkResult
	mustUnmarshal(t, out, &cr)
	if !cr.Declared || len(cr.Checks) != 2 {
		t.Fatalf("forge_check all: %+v", cr)
	}
	if cr.Checks[0].Check != "bad" || cr.Checks[0].Passed || cr.Checks[0].ExitCode != 1 {
		t.Errorf("bad check: %+v", cr.Checks[0])
	}
	if cr.Checks[1].Check != "ok" || !cr.Checks[1].Passed {
		t.Errorf("ok check: %+v", cr.Checks[1])
	}

	// A named check runs alone.
	out, isError = call("forge_check", `{"check":"ok"}`)
	if isError {
		t.Fatalf("forge_check ok: %s", out)
	}
	var one checkResult
	mustUnmarshal(t, out, &one)
	if len(one.Checks) != 1 || one.Checks[0].Check != "ok" || !one.Checks[0].Passed {
		t.Errorf("forge_check named: %+v", one)
	}

	// An unknown name is a tool error with the exact message.
	out, isError = call("forge_check", `{"check":"nope"}`)
	if !isError || string(out) != "check nope is not declared" {
		t.Errorf("forge_check nope: isError=%v %s", isError, out)
	}

	// A daemon tool posts through and returns its output verbatim.
	out, isError = call("forge_note", `{"text":"hi"}`)
	if isError || string(out) != `{"noted":true}` {
		t.Errorf("forge_note: isError=%v %s", isError, out)
	}
	fd.mu.Lock()
	notes := append([]string(nil), fd.noteInputs...)
	fd.mu.Unlock()
	if len(notes) != 1 || notes[0] != `{"text":"hi"}` {
		t.Errorf("daemon received inputs %q", notes)
	}

	// A daemon 404 becomes an isError result carrying the daemon's message.
	out, isError = call("forge_missing", "{}")
	if !isError || string(out) != "no such tool" {
		t.Errorf("forge_missing: isError=%v %s", isError, out)
	}

	// Close drains the sender; every span pair must have arrived.
	bridge.Close()
	wg.Wait()
	fd.mu.Lock()
	batches := append([]protocol.EventBatch(nil), fd.batches...)
	fd.mu.Unlock()
	if len(batches) != len(calls) {
		t.Fatalf("got %d span batches, want %d", len(batches), len(calls))
	}
	seq := 0
	for i, batch := range batches {
		rec := calls[i]
		if batch.Source != protocol.SourceMCP {
			t.Errorf("batch %d source %q", i, batch.Source)
		}
		if len(batch.Events) != 2 {
			t.Fatalf("batch %d has %d events, want the span pair", i, len(batch.Events))
		}
		start, end := batch.Events[0], batch.Events[1]
		for _, ev := range batch.Events {
			if err := ev.Validate(); err != nil {
				t.Errorf("batch %d: %v", i, err)
			}
			if ev.Seq != seq {
				t.Errorf("batch %d: seq %d, want %d", i, ev.Seq, seq)
			}
			seq++
			wantSpan := fmt.Sprintf("mcp-%d", i+1)
			if ev.SpanID != wantSpan || ev.ParentID != "agent-1" || ev.Name != rec.name {
				t.Errorf("batch %d event: span=%s parent=%s name=%s, want %s/agent-1/%s",
					i, ev.SpanID, ev.ParentID, ev.Name, wantSpan, rec.name)
			}
			if ev.Message != "tool_call "+rec.name {
				t.Errorf("batch %d message %q", i, ev.Message)
			}
		}
		if start.Kind != protocol.KindSpanStart || end.Kind != protocol.KindSpanEnd {
			t.Errorf("batch %d kinds %s/%s", i, start.Kind, end.Kind)
		}
		var sa, ea spanAttrs
		mustUnmarshal(t, start.Attrs, &sa)
		mustUnmarshal(t, end.Attrs, &ea)
		if sa.Tool != rec.name || sa.InputBytes != len(rec.input) || sa.InputSHA256 != sum(rec.input) || sa.Clock != "wall" {
			t.Errorf("batch %d start attrs %+v (input %s)", i, sa, rec.input)
		}
		if ea.Tool != rec.name || ea.OutputBytes != len(rec.output) || ea.OutputSHA256 != sum(rec.output) || ea.Clock != "wall" {
			t.Errorf("batch %d end attrs %+v", i, ea)
		}
		if ea.IsError != rec.isError {
			t.Errorf("batch %d is_error %v, want %v", i, ea.IsError, rec.isError)
		}
		if end.DurationUS < 0 {
			t.Errorf("batch %d negative duration", i)
		}
	}
	// The one failing call in the sequence is flagged where expected: the
	// unknown check and the 404 tool.
	var flagged []string
	for i, batch := range batches {
		var ea spanAttrs
		mustUnmarshal(t, batch.Events[1].Attrs, &ea)
		if ea.IsError {
			flagged = append(flagged, calls[i].name)
		}
	}
	if fmt.Sprint(flagged) != fmt.Sprint([]string{"forge_check", "forge_missing"}) {
		t.Errorf("is_error flagged on %v", flagged)
	}
}
