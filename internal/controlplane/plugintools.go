// Plugin tools bridge (DESIGN.md §17): a plugin with the `tools` capability is
// an MCP server on its stdio, and the daemon — which owns the supervised child
// — bridges its tools into the tools registry so every agent's `forge mcp`
// sees them through GET /api/v1/tools and POST /api/v1/tools/{name} unchanged.
//
// The wire is Claude Code's stdio transport: newline-delimited JSON-RPC 2.0
// (internal/mcpserve documents the framing from the server side). The surface
// needed here — initialize, tools/list, tools/call — is small enough to write
// as a client by hand; mcpserve's types are its server's and stay there.
package controlplane

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"strings"
	"sync"
	"time"

	"forge/internal/tools"
)

// pluginToolCallTimeout bounds one forwarded tools/call; a plugin tool slower
// than this is stuck, not slow.
const pluginToolCallTimeout = 60 * time.Second

// pluginInitTimeout bounds the initialize → tools/list handshake after a
// process start.
const pluginInitTimeout = 10 * time.Second

// PluginTools bridges one tools-plugin's MCP stdio into the registry. The
// supervisor hands it the child's pipes after every start (Attach — it is the
// Spec.OnStdio shape); the registry-facing tools forward calls through
// whichever connection is currently live. The tool surface is fixed at the
// first successful list: the registry is static after NewServer, so
// enable/disable changes the tool surface only at the next daemon start.
type PluginTools struct {
	name string
	log  *slog.Logger

	// mu guards conn and defs, and serializes tools/call per plugin.
	mu   sync.Mutex
	conn *mcpConn
	defs []pluginToolDef

	readyOnce sync.Once
	ready     chan struct{}
}

// pluginToolDef is one tool as the plugin advertised it.
type pluginToolDef struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	InputSchema json.RawMessage `json:"inputSchema"`
}

// NewPluginTools builds the bridge for one plugin; Attach connects it.
func NewPluginTools(name string, log *slog.Logger) *PluginTools {
	if log == nil {
		log = slog.New(slog.DiscardHandler)
	}
	return &PluginTools{name: name, log: log, ready: make(chan struct{})}
}

// Attach adopts a freshly started child's stdio: initialize, list its tools,
// and make it the live connection. It matches plugin.Spec.OnStdio and is
// called from the supervisor's goroutine; a failed handshake leaves the
// bridge down until the next restart.
func (p *PluginTools) Attach(stdin io.WriteCloser, stdout io.ReadCloser) {
	conn := newMCPConn(stdin, stdout, p.log)
	ctx, cancel := context.WithTimeout(context.Background(), pluginInitTimeout)
	defer cancel()
	if _, err := conn.call(ctx, "initialize", map[string]any{
		"protocolVersion": "2025-06-18",
		"capabilities":    map[string]any{},
		"clientInfo":      map[string]any{"name": "forge", "version": "daemon"},
	}); err != nil {
		p.log.Warn("plugin mcp initialize failed", "error", err)
		conn.close()
		return
	}
	if err := conn.notify("notifications/initialized", nil); err != nil {
		p.log.Warn("plugin mcp initialized notification failed", "error", err)
		conn.close()
		return
	}
	raw, err := conn.call(ctx, "tools/list", map[string]any{})
	if err != nil {
		p.log.Warn("plugin mcp tools/list failed", "error", err)
		conn.close()
		return
	}
	var listed struct {
		Tools []pluginToolDef `json:"tools"`
	}
	if err := json.Unmarshal(raw, &listed); err != nil {
		p.log.Warn("plugin mcp tools/list undecodable", "error", err)
		conn.close()
		return
	}
	p.mu.Lock()
	if p.conn != nil {
		p.conn.close()
	}
	p.conn = conn
	if p.defs == nil {
		p.defs = listed.Tools
	}
	p.mu.Unlock()
	p.readyOnce.Do(func() { close(p.ready) })
	p.log.Info("plugin tools attached", "tools", len(listed.Tools))
}

// WaitReady blocks until the first successful Attach (or ctx ends) and
// returns the advertised tool definitions.
func (p *PluginTools) WaitReady(ctx context.Context) ([]pluginToolDef, error) {
	select {
	case <-p.ready:
	case <-ctx.Done():
		return nil, fmt.Errorf("plugin %s: tools did not become ready: %w", p.name, ctx.Err())
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.defs, nil
}

// callRemote forwards one tools/call, serialized per plugin.
func (p *PluginTools) callRemote(ctx context.Context, tool string, args json.RawMessage) (json.RawMessage, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.conn == nil || p.conn.dead() {
		return nil, fmt.Errorf("plugin %s is not running", p.name)
	}
	if len(args) == 0 {
		args = json.RawMessage("{}")
	}
	raw, err := p.conn.call(ctx, "tools/call", map[string]any{"name": tool, "arguments": args})
	if err != nil {
		return nil, err
	}
	var res struct {
		Content []struct {
			Type string `json:"type"`
			Text string `json:"text"`
		} `json:"content"`
		IsError bool `json:"isError"`
	}
	if err := json.Unmarshal(raw, &res); err != nil {
		return nil, fmt.Errorf("plugin %s tool %s: undecodable result: %w", p.name, tool, err)
	}
	var text strings.Builder
	for _, c := range res.Content {
		if c.Type == "text" {
			text.WriteString(c.Text)
		}
	}
	if res.IsError {
		return nil, fmt.Errorf("plugin %s tool %s: %s", p.name, tool, text.String())
	}
	out := []byte(text.String())
	if !json.Valid(out) {
		// The registry contract is JSON output; wrap plain text.
		wrapped, err := json.Marshal(map[string]string{"text": text.String()})
		if err != nil {
			return nil, fmt.Errorf("plugin %s tool %s: wrap output: %w", p.name, tool, err)
		}
		out = wrapped
	}
	return out, nil
}

// PluginToolName is the one spelling of the namespacing rule:
// <plugin>_<tool> with '-' replaced by '_' (echo-tools's ping →
// echo_tools_ping).
func PluginToolName(pluginName, toolName string) string {
	return strings.ReplaceAll(pluginName, "-", "_") + "_" + toolName
}

// RegisterPluginTools registers every advertised tool as a daemon tool whose
// Call forwards to the plugin. It runs at daemon start, before NewServer,
// because the registry is static afterwards.
func RegisterPluginTools(ctx context.Context, reg *tools.Registry, p *PluginTools) error {
	defs, err := p.WaitReady(ctx)
	if err != nil {
		return err
	}
	for _, d := range defs {
		schema := d.InputSchema
		if len(schema) == 0 {
			schema = json.RawMessage(`{"type":"object"}`)
		}
		t := pluginTool{
			name: PluginToolName(p.name, d.Name), remote: d.Name,
			description: fmt.Sprintf("[plugin %s] %s", p.name, d.Description),
			schema:      schema, bridge: p,
		}
		if err := reg.Register(t); err != nil {
			return fmt.Errorf("register plugin tool: %w", err)
		}
	}
	return nil
}

// pluginTool adapts one plugin-provided tool to the registry interface.
type pluginTool struct {
	name, remote, description string
	schema                    json.RawMessage
	bridge                    *PluginTools
}

func (t pluginTool) Name() string                 { return t.name }
func (t pluginTool) Description() string          { return t.description }
func (t pluginTool) InputSchema() json.RawMessage { return t.schema }
func (t pluginTool) Where() string                { return tools.WhereDaemon }

func (t pluginTool) Call(ctx context.Context, req tools.Request) (json.RawMessage, error) {
	callCtx, cancel := context.WithTimeout(ctx, pluginToolCallTimeout)
	defer cancel()
	return t.bridge.callRemote(callCtx, t.remote, req.Input)
}

// mcpConn is one live newline-delimited JSON-RPC connection to a child's
// stdio. A single reader goroutine pumps responses to waiting calls; it ends
// at EOF (the child exited), failing everything pending.
type mcpConn struct {
	stdin io.WriteCloser
	log   *slog.Logger

	// mu guards pending, nextID, closed, and writes to stdin.
	mu      sync.Mutex
	pending map[int64]chan rpcReply
	nextID  int64
	closed  bool

	readerDone chan struct{}
}

// rpcReply is what the reader delivers for one id.
type rpcReply struct {
	result json.RawMessage
	err    error
}

func newMCPConn(stdin io.WriteCloser, stdout io.ReadCloser, log *slog.Logger) *mcpConn {
	c := &mcpConn{stdin: stdin, log: log, pending: map[int64]chan rpcReply{}, readerDone: make(chan struct{})}
	go c.read(stdout)
	return c
}

// read is the reader goroutine; close waits for it via readerDone.
func (c *mcpConn) read(stdout io.ReadCloser) {
	defer close(c.readerDone)
	sc := bufio.NewScanner(stdout)
	sc.Buffer(make([]byte, 64<<10), 16<<20)
	for sc.Scan() {
		line := bytes.TrimSpace(sc.Bytes())
		if len(line) == 0 {
			continue
		}
		var resp struct {
			ID     *int64          `json:"id"`
			Result json.RawMessage `json:"result"`
			Error  *struct {
				Code    int    `json:"code"`
				Message string `json:"message"`
			} `json:"error"`
		}
		if err := json.Unmarshal(line, &resp); err != nil || resp.ID == nil {
			continue // a notification or noise; not ours
		}
		c.mu.Lock()
		ch := c.pending[*resp.ID]
		delete(c.pending, *resp.ID)
		c.mu.Unlock()
		if ch == nil {
			continue
		}
		if resp.Error != nil {
			ch <- rpcReply{err: fmt.Errorf("mcp error %d: %s", resp.Error.Code, resp.Error.Message)}
			continue
		}
		ch <- rpcReply{result: resp.Result}
	}
	if err := sc.Err(); err != nil {
		c.log.Debug("plugin mcp read", "error", err)
	}
	// EOF: the child is gone; fail everything pending and mark dead.
	c.mu.Lock()
	c.closed = true
	for id, ch := range c.pending {
		delete(c.pending, id)
		ch <- rpcReply{err: fmt.Errorf("plugin exited mid-call")}
	}
	c.mu.Unlock()
}

func (c *mcpConn) dead() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.closed
}

// call sends one request and waits for its response or ctx.
func (c *mcpConn) call(ctx context.Context, method string, params any) (json.RawMessage, error) {
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return nil, fmt.Errorf("mcp connection closed")
	}
	c.nextID++
	id := c.nextID
	ch := make(chan rpcReply, 1)
	c.pending[id] = ch
	err := c.writeLocked(map[string]any{"jsonrpc": "2.0", "id": id, "method": method, "params": params})
	c.mu.Unlock()
	if err != nil {
		c.mu.Lock()
		delete(c.pending, id)
		c.mu.Unlock()
		return nil, fmt.Errorf("mcp %s: %w", method, err)
	}
	select {
	case r := <-ch:
		if r.err != nil {
			return nil, fmt.Errorf("mcp %s: %w", method, r.err)
		}
		return r.result, nil
	case <-ctx.Done():
		c.mu.Lock()
		delete(c.pending, id)
		c.mu.Unlock()
		return nil, fmt.Errorf("mcp %s: %w", method, ctx.Err())
	}
}

// notify sends one notification (no id, no response).
func (c *mcpConn) notify(method string, params any) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return fmt.Errorf("mcp connection closed")
	}
	return c.writeLocked(map[string]any{"jsonrpc": "2.0", "method": method, "params": params})
}

// writeLocked emits one message as a single line; the caller holds mu.
func (c *mcpConn) writeLocked(v any) error {
	b, err := json.Marshal(v)
	if err != nil {
		return fmt.Errorf("encode: %w", err)
	}
	if _, err := c.stdin.Write(append(b, '\n')); err != nil {
		return err
	}
	return nil
}

// close ends the connection: stdin closes (the child sees EOF) and the reader
// is waited for so no goroutine leaks.
func (c *mcpConn) close() {
	if err := c.stdin.Close(); err != nil {
		c.log.Debug("close plugin stdin", "error", err)
	}
	<-c.readerDone
}
