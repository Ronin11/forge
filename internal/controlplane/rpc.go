package controlplane

// A generic RPC dispatcher: one endpoint, POST /api/v1/rpc/{method}, routes to
// a named handler in a fixed registry. It replaces the pattern of minting a
// bespoke HTTP route for every small UI-triggered action (the test toast was
// the first of what would have been many). Adding an action is now one registry
// entry, not a new route + handler + client wiring.
//
// The registry is the allowlist and the whole security story: only named
// methods are reachable, a UI element carries only the method name — an "RPC
// link", never a URL that could be pointed anywhere — and none of these methods
// decide anything. They notify, re-run, or nudge; anything that commits a
// decision keeps its own audited endpoint. Every call is journaled rpc.invoked.

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"strings"

	"forge/internal/core/store"
)

// rpcFunc runs one named action. args is the raw JSON the caller posted (may be
// empty); the returned body is encoded back. Errors map to status exactly like
// any handler — a store.ErrNotFound-wrapped error is 404, a requestError 400.
type rpcFunc func(ctx context.Context, s *Server, args json.RawMessage) (any, error)

// rpcMethods is the allowlist of RPC methods, keyed by dotted name (group.verb).
// Keep handlers small and side-effect-safe; anything that decides gets its own
// endpoint instead.
var rpcMethods = map[string]rpcFunc{
	"notify.test": rpcNotifyTest,
}

// rpcKnown reports whether a method name is registered — the queue action
// builder uses it so a card can only ever carry a real method.
func rpcKnown(method string) bool {
	_, ok := rpcMethods[method]
	return ok
}

// rpc dispatches POST /api/v1/rpc/{method}: look up the method, journal
// rpc.invoked so every UI-triggered action is auditable, then run it.
func (s *Server) rpc(r *http.Request) (int, any, error) {
	method := r.PathValue("method")
	fn, ok := rpcMethods[method]
	if !ok {
		return 0, nil, fmt.Errorf("rpc method %q: %w", method, store.ErrNotFound)
	}
	var args json.RawMessage
	if r.ContentLength != 0 {
		if err := decodeJSON(r, &args); err != nil {
			return 0, nil, err
		}
	}
	ctx := r.Context()
	if err := s.store.Write(ctx, func(tx *store.Tx) error {
		return tx.Journal(ctx, "rpc.invoked", store.EntityDaemon, method, map[string]any{"method": method})
	}); err != nil {
		return 0, nil, err
	}
	body, err := fn(ctx, s, args)
	if err != nil {
		return 0, nil, err
	}
	if body == nil {
		body = map[string]any{"ok": true, "method": method}
	}
	return http.StatusOK, body, nil
}

// rpcNotifyTest raises a desktop toast whose click routes back into the UI,
// exercising the whole notify chain end to end (journal → plugin → notify-send
// → the omarchy-exec-argv click hint). args may name the UI path the click
// opens; the default is the Human queue the button lives beside. This is the
// former POST /api/v1/notify/test, now a registry entry.
func rpcNotifyTest(ctx context.Context, s *Server, args json.RawMessage) (any, error) {
	path := "/attention"
	if len(args) != 0 {
		var a struct {
			Path string `json:"path"`
		}
		if err := json.Unmarshal(args, &a); err != nil {
			return nil, badRequest("decode args: %v", err)
		}
		if a.Path != "" {
			path = a.Path
		}
	}
	if !strings.HasPrefix(path, "/") || strings.HasPrefix(path, "//") {
		return nil, badRequest("path must be a UI path starting with /")
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error {
		return tx.Journal(ctx, "notify.test", store.EntityDaemon, "notify", map[string]any{"path": path})
	}); err != nil {
		return nil, err
	}
	s.log.InfoContext(ctx, "notify test requested via rpc", "path", path)
	return map[string]any{"queued": true, "path": path}, nil
}
