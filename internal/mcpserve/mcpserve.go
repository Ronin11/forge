// Package mcpserve is the per-attempt MCP server behind `forge mcp` (DESIGN.md
// §13). It exists because every agent process loads Forge's tools through a
// per-attempt MCP config: fact, knowledge, and control tools must execute in the
// daemon while repository tools must run here — a child of the agent, inside the
// worktree and the agent's process group — and every call, wherever it ran, must
// be reported as an `mcp`-source span pair with input/output hashes. No other
// process sits in the right place to do all three.
//
// The MCP wire protocol is implemented by hand: Claude Code's stdio transport is
// newline-delimited JSON-RPC 2.0 (one message per line, no Content-Length
// framing), and the surface Forge needs — initialize, tools/list, tools/call,
// ping — is small enough that the allow-listed github.com/mark3labs/mcp-go
// dependency was declined rather than added (STYLE.md §3 anticipates exactly
// this).
//
// This file is the transport. It knows JSON-RPC and the MCP message shapes and
// nothing about Forge; the bridge (bridge.go) supplies Tools and Call.
package mcpserve

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
)

// fallbackProtocolVersion is answered when the client's initialize request does
// not name a version; otherwise the client's version is echoed, as the MCP
// spec's version negotiation requires of a server that supports it.
const fallbackProtocolVersion = "2025-06-18"

// maxLineBytes bounds one incoming message so a runaway client cannot exhaust
// memory; tool arguments comfortably fit far below it.
const maxLineBytes = 16 << 20

// ToolDef is one tool as advertised in tools/list. InputSchema is a JSON Schema
// object, passed through verbatim.
type ToolDef struct {
	Name        string
	Description string
	InputSchema json.RawMessage
}

// Server serves MCP over a line-delimited JSON-RPC stream. It holds no Forge
// state: Tools is what tools/list advertises and Call executes one tool,
// returning the bytes to place in the result's text content and whether the
// call failed (tool failures are isError results, never JSON-RPC errors).
type Server struct {
	// Version is reported as serverInfo.version in the initialize result.
	Version string
	Tools   []ToolDef
	Call    func(ctx context.Context, name string, args json.RawMessage) (result json.RawMessage, isError bool)
}

// rpcRequest is the decoded shape of one incoming line. ID is kept raw so a
// string or number id is echoed exactly.
type rpcRequest struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params"`
}

// rpcError is the JSON-RPC error object.
type rpcError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

// resultResponse and errorResponse are the two response shapes; they are
// separate structs so an empty result object (`{}` for ping) is never dropped
// by omitempty and a response never carries both members.
type resultResponse struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id"`
	Result  any             `json:"result"`
}

type errorResponse struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id"`
	Error   rpcError        `json:"error"`
}

// serverInfo, serverCapabilities, toolsCapability, and initializeResult are the
// initialize result per the MCP spec.
type serverInfo struct {
	Name    string `json:"name"`
	Version string `json:"version"`
}

type toolsCapability struct {
	ListChanged bool `json:"listChanged"`
}

type serverCapabilities struct {
	Tools toolsCapability `json:"tools"`
}

type initializeResult struct {
	ProtocolVersion string             `json:"protocolVersion"`
	Capabilities    serverCapabilities `json:"capabilities"`
	ServerInfo      serverInfo         `json:"serverInfo"`
}

// toolListItem is one entry of the tools/list result; MCP spells the schema key
// inputSchema, unlike the daemon's snake_case listing.
type toolListItem struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	InputSchema json.RawMessage `json:"inputSchema"`
}

type toolsListResult struct {
	Tools []toolListItem `json:"tools"`
}

// toolContent and toolCallResult are the tools/call result shape.
type toolContent struct {
	Type string `json:"type"`
	Text string `json:"text"`
}

type toolCallResult struct {
	Content []toolContent `json:"content"`
	IsError bool          `json:"isError"`
}

// Serve pumps messages from r to w until EOF (nil), a read/write failure, or
// context cancellation, serving requests one at a time in read order. A read
// blocked on r is not interrupted by ctx; the caller owns r's lifetime (for
// `forge mcp`, the agent closing stdin — or the group kill — ends the stream).
func (s *Server) Serve(ctx context.Context, r io.Reader, w io.Writer) error {
	sc := bufio.NewScanner(r)
	sc.Buffer(make([]byte, 64<<10), maxLineBytes)
	for sc.Scan() {
		if err := ctx.Err(); err != nil {
			return err
		}
		line := bytes.TrimSpace(sc.Bytes())
		if len(line) == 0 {
			continue
		}
		resp := s.handle(ctx, line)
		if resp == nil {
			continue
		}
		if err := writeLine(w, resp); err != nil {
			return fmt.Errorf("write response: %w", err)
		}
	}
	if err := sc.Err(); err != nil {
		return fmt.Errorf("read request stream: %w", err)
	}
	return nil
}

// handle serves one line and returns the response to write, or nil for a
// notification (or a malformed non-request that names no id).
func (s *Server) handle(ctx context.Context, line []byte) any {
	var req rpcRequest
	if err := json.Unmarshal(line, &req); err != nil {
		return errorResponse{JSONRPC: "2.0", ID: json.RawMessage("null"),
			Error: rpcError{Code: -32700, Message: "parse error: " + err.Error()}}
	}
	// A missing or null id marks a notification: it never gets a response,
	// whatever the method.
	if len(req.ID) == 0 || string(req.ID) == "null" {
		return nil
	}
	switch req.Method {
	case "initialize":
		return resultResponse{JSONRPC: "2.0", ID: req.ID, Result: s.initialize(req.Params)}
	case "tools/list":
		items := make([]toolListItem, 0, len(s.Tools))
		for _, t := range s.Tools {
			items = append(items, toolListItem(t))
		}
		return resultResponse{JSONRPC: "2.0", ID: req.ID, Result: toolsListResult{Tools: items}}
	case "tools/call":
		var p struct {
			Name      string          `json:"name"`
			Arguments json.RawMessage `json:"arguments"`
		}
		if err := json.Unmarshal(req.Params, &p); err != nil || p.Name == "" {
			return errorResponse{JSONRPC: "2.0", ID: req.ID,
				Error: rpcError{Code: -32602, Message: "invalid params: tools/call needs a tool name"}}
		}
		out, isError := s.Call(ctx, p.Name, p.Arguments)
		return resultResponse{JSONRPC: "2.0", ID: req.ID,
			Result: toolCallResult{Content: []toolContent{{Type: "text", Text: string(out)}}, IsError: isError}}
	case "ping":
		return resultResponse{JSONRPC: "2.0", ID: req.ID, Result: struct{}{}}
	default:
		return errorResponse{JSONRPC: "2.0", ID: req.ID,
			Error: rpcError{Code: -32601, Message: "method not found: " + req.Method}}
	}
}

// initialize builds the handshake result, echoing the client's protocol version
// when it sent one.
func (s *Server) initialize(params json.RawMessage) initializeResult {
	version := fallbackProtocolVersion
	var p struct {
		ProtocolVersion string `json:"protocolVersion"`
	}
	if err := json.Unmarshal(params, &p); err == nil && p.ProtocolVersion != "" {
		version = p.ProtocolVersion
	}
	return initializeResult{
		ProtocolVersion: version,
		Capabilities:    serverCapabilities{Tools: toolsCapability{ListChanged: false}},
		ServerInfo:      serverInfo{Name: "forge", Version: s.Version},
	}
}

// writeLine emits one response as a single newline-terminated JSON line.
func writeLine(w io.Writer, v any) error {
	b, err := json.Marshal(v)
	if err != nil {
		return fmt.Errorf("encode response: %w", err)
	}
	if _, err := w.Write(append(b, '\n')); err != nil {
		return err
	}
	return nil
}
