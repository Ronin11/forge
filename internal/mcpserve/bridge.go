// The bridge is the Forge side of the MCP server: it fetches the attempt's
// tool listing from the daemon, dispatches each tools/call either back to the
// daemon or to a local repository tool, and reports every call as an
// `mcp`-source span pair (DESIGN.md §8, §13).
//
// The tools listing and call bodies are wire types; they live here rather than
// in internal/protocol because only forge mcp and the daemon's tools handler
// speak them, and this package is the client side being built against the
// contract fixed in DESIGN.md §13.

package mcpserve

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/url"
	"strings"
	"sync/atomic"
	"time"

	"forge/internal/core/protocol"
)

// schemaVersion is the version stamped on every tools request/response body.
const schemaVersion = 1

// Client timeouts: listings and event batches are quick; a tool call may run
// declared checks, so it gets the same ceiling RunChecks allows one check.
const (
	clientTimeout    = 30 * time.Second
	toolCallTimeout  = 15 * time.Minute
	eventSendTimeout = 10 * time.Second
)

// Client is forge mcp's own minimal daemon client. The worker has one too, but
// it carries claim/lease semantics this process must never hold, so this one is
// written against exactly the three routes forge mcp uses.
type Client struct {
	http     *http.Client // listings and event batches
	toolHTTP *http.Client // POST /api/v1/tools/<name>: may run checks
	baseURL  string
	token    string
}

// NewClient builds the client from the environment the worker launched this
// process with: FORGE_SOCKET names the Unix socket, or FORGE_HTTP the loopback
// address; FORGE_TOKEN, when set, is presented as a bearer token.
func NewClient(getenv func(string) string) (*Client, error) {
	transport := &http.Transport{MaxIdleConns: 4, IdleConnTimeout: 90 * time.Second}
	c := &Client{
		http:     &http.Client{Transport: transport, Timeout: clientTimeout},
		toolHTTP: &http.Client{Transport: transport, Timeout: toolCallTimeout},
		token:    getenv("FORGE_TOKEN"),
	}
	switch sock, httpAddr := getenv("FORGE_SOCKET"), getenv("FORGE_HTTP"); {
	case sock != "":
		transport.DialContext = func(ctx context.Context, _, _ string) (net.Conn, error) {
			var d net.Dialer
			return d.DialContext(ctx, "unix", sock)
		}
		c.baseURL = "http://forge"
	case httpAddr != "":
		if !strings.HasPrefix(httpAddr, "http://") {
			httpAddr = "http://" + httpAddr
		}
		c.baseURL = strings.TrimRight(httpAddr, "/")
	default:
		return nil, errors.New("daemon address not set: forge mcp needs FORGE_SOCKET or FORGE_HTTP in its environment")
	}
	return c, nil
}

// statusError is a non-2xx daemon response; unexported because only the bridge
// maps it onto tool results.
type statusError struct {
	status  int
	message string
}

func (e *statusError) Error() string {
	return fmt.Sprintf("daemon returned %d: %s", e.status, e.message)
}

// attemptInfo is the attempt row subset the daemon returns with the listing;
// the local tools run against it so no second source of truth exists.
type attemptInfo struct {
	ID           string `json:"id"`
	TargetID     string `json:"target_id"`
	WorkID       string `json:"work_id"`
	WorktreePath string `json:"worktree_path"`
	Branch       string `json:"branch"`
	BaseBranch   string `json:"base_branch"`
	BaseCommit   string `json:"base_commit"`
	Launches     int    `json:"launches"`
	Mode         string `json:"mode"`
	Autonomy     string `json:"autonomy"`
	Repository   string `json:"repository"`
}

// toolListing is one tool as the daemon lists it: Where says which process
// executes it.
type toolListing struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	InputSchema json.RawMessage `json:"input_schema"`
	Where       string          `json:"where"` // daemon | local
}

type toolsResponse struct {
	SchemaVersion int           `json:"schema_version"`
	Attempt       attemptInfo   `json:"attempt"`
	Tools         []toolListing `json:"tools"`
}

type toolCallRequest struct {
	SchemaVersion int             `json:"schema_version"`
	AttemptID     string          `json:"attempt_id"`
	Input         json.RawMessage `json:"input"`
}

type toolCallResponse struct {
	SchemaVersion int             `json:"schema_version"`
	Output        json.RawMessage `json:"output"`
}

// do performs one JSON request; a nil out discards the body.
func (c *Client) do(ctx context.Context, hc *http.Client, method, path string, in, out any) (err error) {
	var body io.Reader
	if in != nil {
		b, err := json.Marshal(in)
		if err != nil {
			return fmt.Errorf("encode %s %s: %w", method, path, err)
		}
		body = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, c.baseURL+path, body)
	if err != nil {
		return fmt.Errorf("build %s %s: %w", method, path, err)
	}
	if in != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	if c.token != "" {
		req.Header.Set("Authorization", "Bearer "+c.token)
	}
	resp, err := hc.Do(req)
	if err != nil {
		return fmt.Errorf("%s %s: %w", method, path, err)
	}
	defer func() {
		if cerr := resp.Body.Close(); cerr != nil && err == nil {
			err = fmt.Errorf("close response body: %w", cerr)
		}
	}()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		msg, rerr := io.ReadAll(io.LimitReader(resp.Body, 4<<10))
		if rerr != nil {
			return &statusError{status: resp.StatusCode, message: "unreadable error body: " + rerr.Error()}
		}
		var e protocol.Error
		if json.Unmarshal(msg, &e) == nil && e.Error != "" {
			return &statusError{status: resp.StatusCode, message: e.Error}
		}
		return &statusError{status: resp.StatusCode, message: strings.TrimSpace(string(msg))}
	}
	if out == nil {
		if _, err := io.Copy(io.Discard, resp.Body); err != nil {
			return fmt.Errorf("drain %s %s: %w", method, path, err)
		}
		return nil
	}
	if err := json.NewDecoder(resp.Body).Decode(out); err != nil {
		return fmt.Errorf("decode %s %s: %w", method, path, err)
	}
	return nil
}

// tools fetches the attempt's tool listing and attempt info.
func (c *Client) tools(ctx context.Context, attemptID string) (*toolsResponse, error) {
	var out toolsResponse
	path := "/api/v1/tools?attempt_id=" + url.QueryEscape(attemptID)
	if err := c.do(ctx, c.http, http.MethodGet, path, nil, &out); err != nil {
		return nil, err
	}
	return &out, nil
}

// callTool executes one daemon-side tool.
func (c *Client) callTool(ctx context.Context, name, attemptID string, input json.RawMessage) (json.RawMessage, error) {
	req := toolCallRequest{SchemaVersion: schemaVersion, AttemptID: attemptID, Input: input}
	var out toolCallResponse
	if err := c.do(ctx, c.toolHTTP, http.MethodPost, "/api/v1/tools/"+url.PathEscape(name), req, &out); err != nil {
		return nil, err
	}
	return out.Output, nil
}

// events posts one span batch.
func (c *Client) events(ctx context.Context, attemptID string, batch protocol.EventBatch) error {
	return c.do(ctx, c.http, http.MethodPost, "/api/v1/attempts/"+url.PathEscape(attemptID)+"/events", batch, nil)
}

// Bridge dispatches tool calls for one attempt and reports each as a span
// pair. It is the Call function a Server is built with.
type Bridge struct {
	client  *Client
	attempt attemptInfo
	log     *slog.Logger
	tools   []ToolDef
	local   map[string]localTool
	// now supplies wall timestamps (event Time); durations are always measured
	// with the monotonic clock separately.
	now func() time.Time
	// start anchors elapsed_us: this process cannot know attempts.started_at,
	// so elapsed is since the bridge was built and spans carry clock=wall.
	start time.Time
	spanN atomic.Int64
	seq   atomic.Int64
	// batches feeds the single sender goroutine (RunSender), which keeps at
	// most one send in flight; Call queues behind it and drops with a warning
	// only if the queue is full.
	batches chan protocol.EventBatch
}

// NewBridge fetches the tool listing for attemptID and builds the dispatcher.
// A daemon that is unreachable or rejects the attempt surfaces here, before
// the MCP handshake ever starts.
func NewBridge(ctx context.Context, client *Client, attemptID string, log *slog.Logger) (*Bridge, error) {
	listing, err := client.tools(ctx, attemptID)
	if err != nil {
		return nil, fmt.Errorf("fetch tools for attempt %s: %w", attemptID, err)
	}
	b := &Bridge{
		client:  client,
		attempt: listing.Attempt,
		log:     log,
		local:   localTools(),
		now:     time.Now,
		start:   time.Now(),
		batches: make(chan protocol.EventBatch, 64),
	}
	for _, t := range listing.Tools {
		schema := t.InputSchema
		// An absent schema arrives as nothing or JSON null depending on the
		// encoder; either way a local tool falls back to its builtin schema.
		if lt, ok := b.local[t.Name]; t.Where == "local" && ok && (len(schema) == 0 || string(schema) == "null") {
			schema = lt.schema
		}
		b.tools = append(b.tools, ToolDef{Name: t.Name, Description: t.Description, InputSchema: schema})
		if t.Where != "local" {
			// Anything not explicitly local executes in the daemon, including
			// a local-listed tool this build does not implement — the daemon
			// then answers with its own error rather than a silent gap.
			delete(b.local, t.Name)
		}
	}
	return b, nil
}

// Tools is the listing the MCP server advertises.
func (b *Bridge) Tools() []ToolDef { return b.tools }

// Call executes one tool — locally or via the daemon — and queues its span
// pair. It is safe for the transport's one-at-a-time call discipline; it is
// not safe for concurrent calls with Close.
func (b *Bridge) Call(ctx context.Context, name string, args json.RawMessage) (json.RawMessage, bool) {
	input := args
	if len(input) == 0 {
		input = json.RawMessage("{}")
	}
	launches := b.attempt.Launches
	if launches == 0 {
		launches = 1
	}
	spanID := fmt.Sprintf("mcp-%d", b.spanN.Add(1))
	parentID := fmt.Sprintf("agent-%d", launches)
	inSum := sha256.Sum256(input)
	startEvent := protocol.Event{
		Seq:       int(b.seq.Add(1) - 1),
		Time:      b.now().UTC(),
		ElapsedUS: time.Since(b.start).Microseconds(),
		Kind:      protocol.KindSpanStart,
		Message:   "tool_call " + name,
		SpanID:    spanID,
		ParentID:  parentID,
		Name:      name,
		Attrs: encodeAttrs(map[string]any{
			"tool":         name,
			"input_bytes":  len(input),
			"input_sha256": hex.EncodeToString(inSum[:]),
			"clock":        "wall",
		}),
	}
	started := time.Now()

	var out json.RawMessage
	var isError bool
	if lt, ok := b.local[name]; ok {
		out, isError = lt.run(ctx, b, input)
	} else {
		out, isError = b.callDaemon(ctx, name, input)
	}
	if len(out) == 0 {
		out = json.RawMessage("{}")
	}

	outSum := sha256.Sum256(out)
	endEvent := protocol.Event{
		Seq:        int(b.seq.Add(1) - 1),
		Time:       b.now().UTC(),
		ElapsedUS:  time.Since(b.start).Microseconds(),
		Kind:       protocol.KindSpanEnd,
		Message:    "tool_call " + name,
		SpanID:     spanID,
		ParentID:   parentID,
		Name:       name,
		DurationUS: time.Since(started).Microseconds(),
		Attrs: encodeAttrs(map[string]any{
			"tool":          name,
			"output_bytes":  len(out),
			"output_sha256": hex.EncodeToString(outSum[:]),
			"is_error":      isError,
			"clock":         "wall",
		}),
	}
	batch := protocol.EventBatch{Source: protocol.SourceMCP, Events: []protocol.Event{startEvent, endEvent}}
	select {
	case b.batches <- batch:
	default:
		b.log.WarnContext(ctx, "span queue full, dropping batch", "tool", name, "span_id", spanID)
	}
	return out, isError
}

// callDaemon executes a daemon-side tool. A 4xx is the daemon judging the call
// (its message becomes the tool error); anything else is the daemon or the
// transport failing.
func (b *Bridge) callDaemon(ctx context.Context, name string, input json.RawMessage) (json.RawMessage, bool) {
	out, err := b.client.callTool(ctx, name, b.attempt.ID, input)
	if err != nil {
		var se *statusError
		if errors.As(err, &se) && se.status >= 400 && se.status < 500 {
			return json.RawMessage(se.message), true
		}
		return json.RawMessage("daemon error: " + err.Error()), true
	}
	return out, false
}

// RunSender delivers queued span batches one at a time. It returns when Close
// has been called and the queue is drained, or when ctx is cancelled (draining
// what is already queued without blocking). Delivery is fire-and-forget:
// failures are logged at warn and never fail a tool call.
func (b *Bridge) RunSender(ctx context.Context) {
	for {
		select {
		case batch, ok := <-b.batches:
			if !ok {
				return
			}
			b.post(ctx, batch)
		case <-ctx.Done():
			for {
				select {
				case batch, ok := <-b.batches:
					if !ok {
						return
					}
					b.post(ctx, batch)
				default:
					return
				}
			}
		}
	}
}

// Close ends the sender's queue. Call it only after Serve has returned — the
// transport serves calls sequentially, so no Call can still be queueing.
func (b *Bridge) Close() { close(b.batches) }

// post sends one batch with its own deadline, detached from the parent's
// cancellation so a final pair still flushes while this process shuts down;
// parent values (log correlation) are kept.
func (b *Bridge) post(parent context.Context, batch protocol.EventBatch) {
	ctx, cancel := context.WithTimeout(context.WithoutCancel(parent), eventSendTimeout)
	defer cancel()
	if err := b.client.events(ctx, b.attempt.ID, batch); err != nil {
		b.log.WarnContext(ctx, "send span batch", "error", err, "events", len(batch.Events))
	}
}

// encodeAttrs marshals span attrs; the inputs are fixed maps of strings, ints,
// and bools, so failure is impossible — a nil (attrs omitted) is still a valid
// event if it ever were.
func encodeAttrs(m map[string]any) json.RawMessage {
	b, err := json.Marshal(m)
	if err != nil {
		return nil
	}
	return b
}
