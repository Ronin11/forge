// Package tools holds the MCP-exposed functions over Forge state (DESIGN.md
// §13, the M2 tool list): pure functions with stable ordering, explicit
// pagination where lists can grow, numbers pre-computed, and a schema_version
// in every response. Daemon tools run in the control plane behind
// POST /api/v1/tools/{name}; local tools run inside `forge mcp` in the
// worktree and are registered here only so one registry describes the whole
// tool surface. Adding a tool is one file plus a registration (STYLE.md §1).
package tools

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"sort"
	"time"

	"forge/internal/model"
	"forge/internal/store"
)

// SchemaVersion stamps every tool response so agents can detect shape changes.
const SchemaVersion = 1

// Where values: a daemon tool runs in the control plane; a local tool runs
// inside `forge mcp`, a child of the agent in the worktree, so the control
// plane never spawns processes in a worker-owned directory.
const (
	WhereDaemon = "daemon"
	WhereLocal  = "local"
)

// Tool is one MCP-exposed function over Forge state: pure, paginated where
// lists can grow, numbers pre-computed, schema_version in every response.
type Tool interface {
	Name() string
	Description() string
	InputSchema() json.RawMessage // JSON Schema, additionalProperties:false
	Where() string                // "daemon" | "local" (local = runs inside forge mcp)
	// Call is only invoked for daemon tools; local tools return an error.
	Call(ctx context.Context, req Request) (json.RawMessage, error)
}

// Request is one tool invocation; the HTTP handler resolves the attempt
// context before dispatch so every tool sees the same view.
type Request struct {
	AttemptID string
	Attempt   Attempt // resolved by the handler: the attempt + its target/work context
	Input     json.RawMessage
	Deps      Deps
}

// Attempt is the calling attempt's context: the attempt row's own fields plus
// Repository and WorkID from its Target.
type Attempt struct {
	ID, TargetID, WorkID, Repository, WorktreePath, Branch, BaseBranch, BaseCommit, Mode string
	Autonomy                                                                             model.Autonomy
	Launches                                                                             int
}

// Deps are the daemon-side dependencies a tool call may use; the handler
// builds them from the server so tools hold no state of their own.
type Deps struct {
	Store  *store.Store
	Write  func(ctx context.Context, fn func(*store.Tx) error) error // = Store.Write
	KbDir  string
	Clock  func() time.Time
	Logger *slog.Logger
}

// InputError is a tool input the caller got wrong; the handler maps it to 400
// while store sentinels keep their usual statuses.
type InputError struct{ msg string }

func (e *InputError) Error() string { return e.msg }

// BadInput builds an InputError.
func BadInput(format string, args ...any) error {
	return &InputError{msg: fmt.Sprintf(format, args...)}
}

// IsBadInput reports whether err is (or wraps) an InputError.
func IsBadInput(err error) bool {
	var ie *InputError
	return errors.As(err, &ie)
}

// Registry maps tool names to implementations. It is a value passed
// explicitly (STYLE.md §1); there is no package-level registry.
type Registry struct {
	byName map[string]Tool
}

// NewRegistry returns an empty registry.
func NewRegistry() *Registry { return &Registry{byName: map[string]Tool{}} }

// Register adds one tool; a duplicate name is an error so two registrations
// never shadow each other silently.
func (r *Registry) Register(t Tool) error {
	if _, ok := r.byName[t.Name()]; ok {
		return fmt.Errorf("tool %s already registered", t.Name())
	}
	r.byName[t.Name()] = t
	return nil
}

// Get looks a tool up by name.
func (r *Registry) Get(name string) (Tool, bool) {
	t, ok := r.byName[name]
	return t, ok
}

// All returns every tool sorted by name — the stable order of GET /api/v1/tools.
func (r *Registry) All() []Tool {
	out := make([]Tool, 0, len(r.byName))
	for _, t := range r.byName {
		out = append(out, t)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name() < out[j].Name() })
	return out
}

// Defaults registers the M2 tool set: facts, knowledge, control, and the
// local repository tools `forge mcp` executes itself.
func Defaults() *Registry {
	r := NewRegistry()
	all := []Tool{
		usageTool{}, attemptTool{}, eventsTool{}, promptVersionTool{}, queueTool{},
		statsTool{}, retroPackTool{},
		kbSearchTool{}, kbNoteTool{}, kbNewTool{}, kbBacklinksTool{}, kbLinksTool{},
		askTool{}, noteProgressTool{}, proposeTool{},
	}
	all = append(all, localTools()...)
	for _, t := range all {
		if err := r.Register(t); err != nil {
			// Names are compile-time constants; a duplicate is a programming
			// error, caught by the tests, not a runtime condition.
			panic("tools: " + err.Error())
		}
	}
	return r
}

// decodeInput reads a tool's input strictly: unknown fields are the caller's
// mistake (the schemas say additionalProperties:false), not something to
// ignore. An empty input means {}.
func decodeInput(raw json.RawMessage, v any) error {
	if len(raw) == 0 {
		raw = json.RawMessage("{}")
	}
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.DisallowUnknownFields()
	if err := dec.Decode(v); err != nil {
		return BadInput("input: %v", err)
	}
	return nil
}

// respond marshals a response body, which already carries schema_version.
func respond(v any) (json.RawMessage, error) {
	b, err := json.Marshal(v)
	if err != nil {
		return nil, fmt.Errorf("encode tool response: %w", err)
	}
	return b, nil
}

// page is the shared pagination input of every list tool.
type page struct {
	Limit  int `json:"limit"`
	Offset int `json:"offset"`
}

// clamp applies the list bounds: limit default 50, max 200; offset ≥ 0.
func (p *page) clamp() error {
	if p.Limit < 0 || p.Offset < 0 {
		return BadInput("limit and offset must not be negative")
	}
	if p.Limit == 0 {
		p.Limit = 50
	}
	if p.Limit > 200 {
		p.Limit = 200
	}
	return nil
}

// slicePage applies a page to a fetched, already-ordered list.
func slicePage[T any](items []T, p page) []T {
	if p.Offset >= len(items) {
		return []T{}
	}
	items = items[p.Offset:]
	if len(items) > p.Limit {
		items = items[:p.Limit]
	}
	return items
}

// pageProps is the JSON-Schema fragment every list tool's schema includes.
const pageProps = `"limit":{"type":"integer","minimum":0,"maximum":200,"description":"max rows; default 50"},"offset":{"type":"integer","minimum":0,"description":"rows to skip"}`

// emptySchema is the schema of a tool that takes no input.
const emptySchema = `{"type":"object","properties":{},"additionalProperties":false}`
