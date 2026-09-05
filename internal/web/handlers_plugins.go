package web

import (
	"context"
	"net/http"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/plugin"
	"forge/internal/core/store"
)

// pluginRoutes registers the plugin surface (DESIGN.md §17): the journal feed
// (JSON list and SSE stream) for events plugins, the ack cursor, the
// list/install/enable/disable endpoints behind `forge plugin`, and the
// external-refs annotation route.
func (s *Server) pluginRoutes(m *http.ServeMux) {
	m.HandleFunc("GET /api/v1/journal", s.journalRoute)
	m.HandleFunc("GET /api/v1/plugins", s.handle(s.listPlugins))
	m.HandleFunc("GET /api/v1/plugins/{name}", s.handle(s.getPluginInfo))
	m.HandleFunc("POST /api/v1/plugins/install", s.handle(s.installPlugin))
	m.HandleFunc("DELETE /api/v1/plugins/{name}", s.handle(s.uninstallPlugin))
	m.HandleFunc("POST /api/v1/plugins/{name}/enable", s.handle(s.enablePlugin))
	m.HandleFunc("POST /api/v1/plugins/{name}/disable", s.handle(s.disablePlugin))
	m.HandleFunc("POST /api/v1/plugins/{name}/ack", s.handle(s.ackPlugin))
	for _, base := range []string{"/api/v1/work", "/api/v1/tasks"} {
		m.HandleFunc("POST "+base+"/{id}/external-refs", s.handle(s.annotateWork))
	}
}

// ---- plugin token auth and the scope gate ----

// pluginCtxKey carries the authenticated plugin (name and scopes) through the
// request context once the middleware has resolved a plugin bearer token.
type pluginCtxKey struct{}

// pluginFrom reads the authenticated plugin off a request context; nil means
// the caller is not a plugin (operator or worker).
func pluginFrom(ctx context.Context) *store.Plugin {
	if p, ok := ctx.Value(pluginCtxKey{}).(*store.Plugin); ok {
		return p
	}
	return nil
}

// pluginForRequest resolves a bearer token that is not the worker token to an
// enabled plugin. Anything else — no bearer, the worker token, an unknown
// token (an attempt's MCP token on the tool routes) — returns nil and keeps
// today's behavior exactly.
func (s *Server) pluginForRequest(r *http.Request) *store.Plugin {
	presented, ok := strings.CutPrefix(r.Header.Get("Authorization"), "Bearer ")
	if !ok || presented == "" || s.tokenOK(r) {
		return nil
	}
	p, err := s.store.PluginByTokenHash(r.Context(), plugin.HashToken(presented))
	if err != nil {
		return nil
	}
	return p
}

// pluginRouteScope is one row of the scope table: which routes a scope
// unlocks. Methods is a space-separated set; a Prefix ending in "/" matches
// any deeper path, otherwise the prefix must end at a path boundary (so
// /api/v1/work never captures /api/v1/workers). More specific rows come first;
// the first match wins.
type pluginRouteScope struct {
	Methods string
	Prefix  string
	Scope   string
}

// pluginScopeTable is the one home for the plugin route → scope rule
// (DESIGN.md §17). A route not listed here is outside every plugin's reach.
var pluginScopeTable = []pluginRouteScope{
	{"POST", "/api/v1/work/{id}/external-refs", plugin.ScopeAnnotateWrite},
	{"POST", "/api/v1/tasks/{id}/external-refs", plugin.ScopeAnnotateWrite},
	{"GET", "/api/v1/journal", plugin.ScopeEventsRead},
	{"GET", "/api/v1/work", plugin.ScopeWorkRead},
	{"GET", "/api/v1/tasks", plugin.ScopeWorkRead},
	{"GET", "/api/v1/queue", plugin.ScopeWorkRead},
	{"GET", "/api/v1/attention", plugin.ScopeWorkRead},
	{"POST PATCH DELETE", "/api/v1/work", plugin.ScopeWorkWrite},
	// The assistant route turns a channel message into tasks/status/replies —
	// work creation by another name, so work:write governs it. Missing from
	// this table, it silently cut every channel bridge off from the chat
	// revamp until Signal messages went unanswered (2026-09-05).
	{"POST", "/api/v1/assistant/message", plugin.ScopeWorkWrite},
	{"POST PATCH DELETE", "/api/v1/tasks", plugin.ScopeWorkWrite},
	{"POST", "/api/v1/questions/", plugin.ScopeWorkWrite},
	{"GET", "/api/v1/usage", plugin.ScopeUsageRead},
	{"GET", "/api/v1/kb/", plugin.ScopeKbRead},
	{"POST", "/api/v1/kb/", plugin.ScopeKbWrite},
	{"GET", "/api/v1/proposals", plugin.ScopeProposalRead},
}

// pluginRequiredScope maps one request to the scope it needs; !ok means no
// plugin may call it at all.
func pluginRequiredScope(method, path string) (string, bool) {
	for _, row := range pluginScopeTable {
		if !strings.Contains(" "+row.Methods+" ", " "+method+" ") {
			continue
		}
		if matchScopePrefix(path, row.Prefix) {
			return row.Scope, true
		}
	}
	return "", false
}

// matchScopePrefix matches a table prefix against a concrete path; the "{id}"
// segment matches exactly one path element.
func matchScopePrefix(path, prefix string) bool {
	if i := strings.Index(prefix, "{id}"); i >= 0 {
		head, tail := prefix[:i], prefix[i+len("{id}"):]
		if !strings.HasPrefix(path, head) {
			return false
		}
		rest := path[len(head):]
		slash := strings.IndexByte(rest, '/')
		if slash < 0 {
			return false
		}
		return rest[slash:] == tail
	}
	if strings.HasSuffix(prefix, "/") {
		return strings.HasPrefix(path, prefix)
	}
	return path == prefix || strings.HasPrefix(path, prefix+"/")
}

// pluginAuthorized is the gate the middleware applies to every request that
// presented a plugin token — on the unix socket too, because plugins connect
// over the socket.
func pluginAuthorized(p *store.Plugin, method, path string) bool {
	// A plugin may always acknowledge its own cursor; anyone else's is not its.
	if method == http.MethodPost && path == "/api/v1/plugins/"+p.Name+"/ack" {
		return true
	}
	scope, ok := pluginRequiredScope(method, path)
	if !ok {
		return false
	}
	for _, s := range p.Scopes {
		if s == scope {
			return true
		}
	}
	return false
}

// journalPluginDenied writes the one audit row a scope refusal leaves.
func (s *Server) journalPluginDenied(ctx context.Context, name, path string) {
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		return tx.Journal(ctx, "plugin.denied", store.EntityPlugin, name, map[string]any{"plugin": name, "path": path})
	})
	if err != nil {
		s.log.WarnContext(ctx, "journal plugin denial", "plugin", name, "error", err)
	}
}

// ---- journal: JSON list or SSE stream ----

// journalRoute keeps GET /api/v1/journal's JSON list and adds the SSE mode
// events plugins follow (?follow=1&since=<id>, or Accept: text/event-stream).
func (s *Server) journalRoute(w http.ResponseWriter, r *http.Request) {
	if r.URL.Query().Get("follow") == "1" || strings.Contains(r.Header.Get("Accept"), "text/event-stream") {
		s.streamJournal(w, r)
		return
	}
	s.handle(s.journal)(w, r)
}

// streamJournal serves the whole journal as SSE from a cursor: `event:
// journal`, id = journal id, data = the entry, polled every streamInterval.
// Like streamWork it bypasses handle — it owns its writer and must not count
// as in-flight — and a drain or shutdown closes it with a retry hint so a
// plugin reconnects from its acked cursor (DESIGN.md §17).
func (s *Server) streamJournal(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	var since int64
	if raw := r.URL.Query().Get("since"); raw != "" {
		n, err := strconv.ParseInt(raw, 10, 64)
		if err != nil || n < 0 {
			s.fail(ctx, w, badRequest("since %q: want a journal id", raw))
			return
		}
		since = n
	}
	rc := http.NewResponseController(w)
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.WriteHeader(http.StatusOK)
	if err := rc.Flush(); err != nil {
		s.log.WarnContext(ctx, "journal stream: response writer cannot flush", "error", err)
		return
	}
	s.log.DebugContext(ctx, "journal stream opened", "since", since)
	for {
		entries, err := s.store.JournalSince(ctx, since, streamBatch)
		if err != nil {
			s.log.WarnContext(ctx, "journal stream: read journal", "error", err)
			return
		}
		for _, e := range entries {
			if err := writeSSE(w, "journal", e.ID, e); err != nil {
				s.log.DebugContext(ctx, "journal stream ended", "error", err)
				return
			}
			since = e.ID
		}
		if len(entries) > 0 {
			if err := rc.Flush(); err != nil {
				return
			}
			continue // drain the backlog before sleeping
		}
		if s.Draining() {
			s.streamRetryHint(ctx, w, rc)
			return
		}
		select {
		case <-ctx.Done():
			return
		case <-s.closed:
			s.streamRetryHint(ctx, w, rc)
			return
		case <-time.After(s.streamInterval):
		}
	}
}

// ---- plugin management ----

// pluginInfo is one row of GET /api/v1/plugins: the store row merged with the
// supervisor's live health.
type pluginInfo struct {
	Name        string    `json:"name"`
	Version     string    `json:"version"`
	Kind        string    `json:"kind"`
	Path        string    `json:"path"`
	Enabled     bool      `json:"enabled"`
	Scopes      []string  `json:"scopes"`
	Cursor      int64     `json:"cursor"`
	InstalledAt time.Time `json:"installed_at"`
	EnabledAt   time.Time `json:"enabled_at,omitempty"`
	Running     bool      `json:"running"`
	PID         int       `json:"pid,omitempty"`
	Restarts    int       `json:"restarts"`
	LastExit    string    `json:"last_exit,omitempty"`
	Since       time.Time `json:"since,omitempty"`
}

// healthByName snapshots the supervisor; nil-safe for servers without one.
func (s *Server) healthByName() map[string]plugin.PluginHealth {
	out := map[string]plugin.PluginHealth{}
	if s.pluginHealth == nil {
		return out
	}
	for _, h := range s.pluginHealth() {
		out[h.Name] = h
	}
	return out
}

func mergePluginInfo(p store.Plugin, h plugin.PluginHealth) pluginInfo {
	return pluginInfo{
		Name: p.Name, Version: p.Version, Kind: p.Kind, Path: p.Path, Enabled: p.Enabled,
		Scopes: p.Scopes, Cursor: p.Cursor, InstalledAt: p.InstalledAt, EnabledAt: p.EnabledAt,
		Running: h.Running, PID: h.PID, Restarts: h.Restarts, LastExit: h.LastExit, Since: h.Since,
	}
}

func (s *Server) listPlugins(r *http.Request) (int, any, error) {
	rows, err := s.store.Plugins(r.Context())
	if err != nil {
		return 0, nil, err
	}
	health := s.healthByName()
	out := make([]pluginInfo, 0, len(rows))
	for _, p := range rows {
		out = append(out, mergePluginInfo(p, health[p.Name]))
	}
	return http.StatusOK, out, nil
}

func (s *Server) getPluginInfo(r *http.Request) (int, any, error) {
	name, err := pathName(r)
	if err != nil {
		return 0, nil, err
	}
	p, err := s.store.GetPlugin(r.Context(), name)
	if err != nil {
		return 0, nil, err
	}
	return http.StatusOK, mergePluginInfo(*p, s.healthByName()[name]), nil
}

// installPluginBody is POST /api/v1/plugins/install: the CLI has already
// placed the plugin directory under <home>/plugins; the daemon validates the
// manifest there and records the row.
type installPluginBody struct {
	Name string `json:"name"`
	Kind string `json:"kind"` // first_party | third_party (default)
}

func (s *Server) installPlugin(r *http.Request) (int, any, error) {
	ctx := r.Context()
	var body installPluginBody
	if err := decodeJSON(r, &body); err != nil {
		return 0, nil, err
	}
	if err := model.ValidateName(body.Name); err != nil {
		return 0, nil, badRequest("plugin name: %v", err)
	}
	switch body.Kind {
	case "":
		body.Kind = "third_party"
	case "first_party", "third_party":
	default:
		return 0, nil, badRequest("kind %q: want first_party or third_party", body.Kind)
	}
	if s.home == "" {
		return 0, nil, badRequest("this daemon has no home directory; plugins cannot be installed")
	}
	// Resolve the plugin across the discovery roots (earlier wins): the CLI has
	// placed a first-party copy under <home>/plugins, but an out-of-tree plugin
	// lives in a configured plugin_dir and is registered in place.
	roots := s.pluginRoots
	if len(roots) == 0 {
		roots = []string{filepath.Join(s.home, "plugins")}
	}
	m, err := plugin.LoadFromRoots(roots, body.Name)
	if err != nil {
		return 0, nil, badRequest("%v", err)
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		return tx.InstallPlugin(ctx, store.Plugin{Name: m.Name, Version: m.Version, Kind: body.Kind, Path: m.Dir, Scopes: m.Scopes})
	})
	if err != nil {
		return 0, nil, err
	}
	p, err := s.store.GetPlugin(ctx, m.Name)
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "plugin installed", "plugin", m.Name, "version", m.Version, "kind", body.Kind)
	return http.StatusOK, mergePluginInfo(*p, s.healthByName()[m.Name]), nil
}

func (s *Server) uninstallPlugin(r *http.Request) (int, any, error) {
	ctx := r.Context()
	name, err := pathName(r)
	if err != nil {
		return 0, nil, err
	}
	if s.pluginStop != nil {
		s.pluginStop(name)
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.UninstallPlugin(ctx, name) }); err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "plugin uninstalled", "plugin", name)
	return http.StatusNoContent, nil, nil
}

// enablePlugin marks the plugin enabled and mints its token. The token never
// leaves the daemon and never rests on disk: only its sha256 is stored, and
// the daemon re-mints a fresh one (refreshing the hash the same way) every
// time it starts the plugin's process — so the enable response carries no
// secret, and a daemon restart invalidates old tokens by construction.
func (s *Server) enablePlugin(r *http.Request) (int, any, error) {
	ctx := r.Context()
	name, err := pathName(r)
	if err != nil {
		return 0, nil, err
	}
	token, hash, err := plugin.NewToken()
	if err != nil {
		return 0, nil, err
	}
	var row *store.Plugin
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		var werr error
		row, werr = tx.EnablePlugin(ctx, name, hash)
		return werr
	})
	if err != nil {
		return 0, nil, err
	}
	if s.pluginStart != nil {
		if err := s.pluginStart(name, token); err != nil {
			s.log.WarnContext(ctx, "plugin enabled but did not start", "plugin", name, "error", err)
			info := mergePluginInfo(*row, s.healthByName()[name])
			info.LastExit = "start failed: " + err.Error()
			return http.StatusOK, info, nil
		}
	}
	s.log.InfoContext(ctx, "plugin enabled", "plugin", name, "scopes", strings.Join(row.Scopes, ","))
	return http.StatusOK, mergePluginInfo(*row, s.healthByName()[name]), nil
}

func (s *Server) disablePlugin(r *http.Request) (int, any, error) {
	ctx := r.Context()
	name, err := pathName(r)
	if err != nil {
		return 0, nil, err
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.DisablePlugin(ctx, name) }); err != nil {
		return 0, nil, err
	}
	if s.pluginStop != nil {
		s.pluginStop(name)
	}
	s.log.InfoContext(ctx, "plugin disabled", "plugin", name)
	return http.StatusNoContent, nil, nil
}

// ackBody is POST /api/v1/plugins/{name}/ack.
type ackBody struct {
	Cursor int64 `json:"cursor"`
}

// ackPlugin persists a plugin's journal cursor. The middleware already
// restricts a plugin token to its own name; the belt-and-braces check here
// covers servers exercised without the middleware. An operator (no bearer)
// may ack on a plugin's behalf.
func (s *Server) ackPlugin(r *http.Request) (int, any, error) {
	ctx := r.Context()
	name, err := pathName(r)
	if err != nil {
		return 0, nil, err
	}
	if p := pluginFrom(ctx); p != nil && p.Name != name {
		return http.StatusForbidden, nil, badRequest("plugin %s cannot ack for %s", p.Name, name)
	}
	var body ackBody
	if err := decodeJSON(r, &body); err != nil {
		return 0, nil, err
	}
	if body.Cursor < 0 {
		return 0, nil, badRequest("cursor %d: want a journal id", body.Cursor)
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.AckPluginCursor(ctx, name, body.Cursor) }); err != nil {
		return 0, nil, err
	}
	return http.StatusNoContent, nil, nil
}

// ---- external refs (annotate) ----

// annotateBody is POST /api/v1/work/{id}/external-refs.
type annotateBody struct {
	Refs []store.ExternalRef `json:"refs"`
}

func (s *Server) annotateWork(r *http.Request) (int, any, error) {
	ctx := r.Context()
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	var body annotateBody
	if err := decodeJSON(r, &body); err != nil {
		return 0, nil, err
	}
	if len(body.Refs) == 0 || len(body.Refs) > 50 {
		return 0, nil, badRequest("refs: want 1–50 entries")
	}
	caller := pluginFrom(ctx)
	for i, ref := range body.Refs {
		if caller != nil && ref.Plugin == "" {
			body.Refs[i].Plugin = caller.Name
			ref.Plugin = caller.Name
		}
		if caller != nil && ref.Plugin != caller.Name {
			return 0, nil, badRequest("refs[%d]: plugin %q is not the caller", i, ref.Plugin)
		}
		if ref.Plugin == "" || ref.Kind == "" || ref.ID == "" {
			return 0, nil, badRequest("refs[%d]: plugin, kind, and id are required", i)
		}
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.AddWorkExternalRefs(ctx, id, body.Refs) }); err != nil {
		return 0, nil, err
	}
	who := "operator"
	if caller != nil {
		who = caller.Name
	}
	s.log.InfoContext(ctx, "work annotated", "work_id", id, "refs", len(body.Refs), "by", who)
	return http.StatusNoContent, nil, nil
}

// pathName reads a {name} segment and validates it as a Forge name before it
// reaches a query or a log line.
func pathName(r *http.Request) (string, error) {
	name := r.PathValue("name")
	if err := model.ValidateName(name); err != nil {
		return "", badRequest("%v", err)
	}
	return name, nil
}
