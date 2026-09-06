package web

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/plugin"
	"forge/internal/core/store"
)

// pluginServer is a store + server + httptest listener tuned for the plugin
// tests: a real home for install, a fast SSE poll, and configurable hooks.
type pluginServer struct {
	t    *testing.T
	st   *store.Store
	srv  *Server
	http *httptest.Server
	home string
}

func newPluginServer(t *testing.T, transport string, mutate func(*ServerOptions)) *pluginServer {
	t.Helper()
	home := t.TempDir()
	st, err := store.Open(context.Background(), filepath.Join(home, "forge.sqlite3"), store.Options{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	})
	opts := ServerOptions{
		Store: st, Version: "test", Token: testToken, TransportOverride: transport,
		Home: home, StreamInterval: 20 * time.Millisecond,
	}
	if mutate != nil {
		mutate(&opts)
	}
	srv, err := NewServer(opts)
	if err != nil {
		t.Fatal(err)
	}
	hs := httptest.NewServer(srv.Handler())
	t.Cleanup(hs.Close)
	return &pluginServer{t: t, st: st, srv: srv, http: hs, home: home}
}

// installEnabled installs one plugin row and enables it with a fresh token,
// returning the plain token a request would present.
func (ps *pluginServer) installEnabled(name string, scopes []string) string {
	ps.t.Helper()
	token, hash, err := plugin.NewToken()
	if err != nil {
		ps.t.Fatal(err)
	}
	ctx := context.Background()
	err = ps.st.Write(ctx, func(tx *store.Tx) error {
		if err := tx.InstallPlugin(ctx, store.Plugin{Name: name, Version: "0.1.0", Kind: "third_party", Path: "/x/" + name, Scopes: scopes}); err != nil {
			return err
		}
		_, err := tx.EnablePlugin(ctx, name, hash)
		return err
	})
	if err != nil {
		ps.t.Fatal(err)
	}
	return token
}

func (ps *pluginServer) do(method, path, token string, body any) (int, []byte) {
	ps.t.Helper()
	var reader *bytes.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			ps.t.Fatal(err)
		}
		reader = bytes.NewReader(b)
	} else {
		reader = bytes.NewReader(nil)
	}
	req, err := http.NewRequest(method, ps.http.URL+path, reader)
	if err != nil {
		ps.t.Fatal(err)
	}
	req.Header.Set("Content-Type", "application/json")
	if token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
	}
	resp, err := ps.http.Client().Do(req)
	if err != nil {
		ps.t.Fatal(err)
	}
	var buf bytes.Buffer
	if _, err := buf.ReadFrom(resp.Body); err != nil {
		ps.t.Fatal(err)
	}
	if err := resp.Body.Close(); err != nil {
		ps.t.Fatal(err)
	}
	return resp.StatusCode, buf.Bytes()
}

func (ps *pluginServer) journalKinds() []string {
	ps.t.Helper()
	entries, err := ps.st.JournalSince(context.Background(), 0, 1000)
	if err != nil {
		ps.t.Fatal(err)
	}
	kinds := make([]string, len(entries))
	for i, e := range entries {
		kinds[i] = e.Kind
	}
	return kinds
}

func TestPluginScopeGate(t *testing.T) {
	t.Parallel()
	for _, transport := range []string{transportUnix, transportTCP} {
		t.Run(transport, func(t *testing.T) {
			t.Parallel()
			ps := newPluginServer(t, transport, nil)
			tokEvents := ps.installEnabled("eventer", []string{plugin.ScopeEventsRead})
			tokWork := ps.installEnabled("writer", []string{plugin.ScopeWorkRead, plugin.ScopeWorkWrite})

			if status, body := ps.do(http.MethodGet, "/api/v1/journal", tokEvents, nil); status != http.StatusOK {
				t.Errorf("journal with events:read = %d %s, want 200", status, body)
			}
			status, body := ps.do(http.MethodGet, "/api/v1/work", tokEvents, nil)
			if status != http.StatusForbidden || !strings.Contains(string(body), "outside plugin scopes") {
				t.Errorf("work with events:read = %d %s, want 403", status, body)
			}
			if status, body := ps.do(http.MethodGet, "/api/v1/work", tokWork, nil); status != http.StatusOK {
				t.Errorf("work with work:read = %d %s, want 200", status, body)
			}
			// Prefix boundary: /api/v1/work must not unlock /api/v1/workers.
			if status, _ := ps.do(http.MethodGet, "/api/v1/workers", tokWork, nil); status != http.StatusForbidden {
				t.Errorf("workers with a plugin token = %d, want 403", status)
			}
			// The denial is journaled once per refusal.
			denied := 0
			for _, k := range ps.journalKinds() {
				if k == "plugin.denied" {
					denied++
				}
			}
			if denied != 2 {
				t.Errorf("plugin.denied journal rows = %d, want 2", denied)
			}
			// No bearer keeps today's behavior (unix operator / open TCP reads).
			if status, _ := ps.do(http.MethodGet, "/api/v1/work", "", nil); status != http.StatusOK {
				t.Errorf("work without token = %d, want 200", status)
			}
			// The worker token keeps full access, untouched by the scope gate.
			if status, _ := ps.do(http.MethodGet, "/api/v1/journal", testToken, nil); status != http.StatusOK {
				t.Errorf("journal with worker token = %d, want 200", status)
			}
		})
	}
}

func TestPluginScopeTable(t *testing.T) {
	t.Parallel()
	cases := []struct {
		method, path string
		scope        string
		ok           bool
	}{
		{"GET", "/api/v1/journal", plugin.ScopeEventsRead, true},
		{"GET", "/api/v1/work", plugin.ScopeWorkRead, true},
		{"GET", "/api/v1/work/0123456789abcdef0123456789abcdef", plugin.ScopeWorkRead, true},
		{"GET", "/api/v1/tasks", plugin.ScopeWorkRead, true},
		{"GET", "/api/v1/queue", plugin.ScopeWorkRead, true},
		{"GET", "/api/v1/attention", plugin.ScopeWorkRead, true},
		{"POST", "/api/v1/work", plugin.ScopeWorkWrite, true},
		{"PATCH", "/api/v1/work/0123456789abcdef0123456789abcdef", plugin.ScopeWorkWrite, true},
		{"DELETE", "/api/v1/tasks/0123456789abcdef0123456789abcdef", plugin.ScopeWorkWrite, true},
		{"POST", "/api/v1/questions/0123456789abcdef0123456789abcdef/answer", plugin.ScopeWorkWrite, true},
		{"POST", "/api/v1/assistant/message", plugin.ScopeWorkWrite, true},
		{"GET", "/api/v1/questions/0123456789abcdef0123456789abcdef", plugin.ScopeWorkRead, true},
		{"GET", "/api/v1/assistant/message", "", false},
		{"GET", "/api/v1/usage", plugin.ScopeUsageRead, true},
		{"GET", "/api/v1/kb/search", plugin.ScopeKbRead, true},
		{"POST", "/api/v1/kb/reindex", plugin.ScopeKbWrite, true},
		{"GET", "/api/v1/proposals", plugin.ScopeProposalRead, true},
		{"POST", "/api/v1/work/0123456789abcdef0123456789abcdef/external-refs", plugin.ScopeAnnotateWrite, true},
		{"POST", "/api/v1/tasks/0123456789abcdef0123456789abcdef/external-refs", plugin.ScopeAnnotateWrite, true},
		{"GET", "/api/v1/workers", "", false},
		{"POST", "/api/v1/worker/claim", "", false},
		{"POST", "/api/v1/journal", "", false},
		{"GET", "/api/v1/plugins", "", false},
		{"POST", "/api/v1/tools/forge_usage", "", false},
	}
	for _, c := range cases {
		scope, ok := pluginRequiredScope(c.method, c.path)
		if ok != c.ok || scope != c.scope {
			t.Errorf("pluginRequiredScope(%s %s) = %q,%v; want %q,%v", c.method, c.path, scope, ok, c.scope, c.ok)
		}
	}
}

func TestPluginAck(t *testing.T) {
	t.Parallel()
	ps := newPluginServer(t, transportUnix, nil)
	tokE := ps.installEnabled("eventer", []string{plugin.ScopeEventsRead})
	tokOther := ps.installEnabled("other", []string{plugin.ScopeEventsRead})

	if status, body := ps.do(http.MethodPost, "/api/v1/plugins/eventer/ack", tokE, map[string]int64{"cursor": 7}); status != http.StatusNoContent {
		t.Fatalf("own ack = %d %s, want 204", status, body)
	}
	p, err := ps.st.GetPlugin(context.Background(), "eventer")
	if err != nil {
		t.Fatal(err)
	}
	if p.Cursor != 7 {
		t.Errorf("cursor = %d, want 7", p.Cursor)
	}
	// A stale ack never moves the cursor backwards.
	if status, _ := ps.do(http.MethodPost, "/api/v1/plugins/eventer/ack", tokE, map[string]int64{"cursor": 3}); status != http.StatusNoContent {
		t.Error("stale ack should be a quiet no-op")
	}
	if p, err = ps.st.GetPlugin(context.Background(), "eventer"); err != nil || p.Cursor != 7 {
		t.Errorf("cursor after stale ack = %d (err %v), want 7", p.Cursor, err)
	}
	// Another plugin cannot ack for eventer.
	if status, _ := ps.do(http.MethodPost, "/api/v1/plugins/eventer/ack", tokOther, map[string]int64{"cursor": 9}); status != http.StatusForbidden {
		t.Error("cross-plugin ack should be 403")
	}
	// An operator (no bearer) may ack on a plugin's behalf.
	if status, _ := ps.do(http.MethodPost, "/api/v1/plugins/eventer/ack", "", map[string]int64{"cursor": 9}); status != http.StatusNoContent {
		t.Error("operator ack should be 204")
	}
}

func TestPluginEnableMintsTokenAndStartsProcess(t *testing.T) {
	t.Parallel()
	var startedName, startedToken string
	stopped := []string{}
	ps := newPluginServer(t, transportUnix, func(o *ServerOptions) {
		o.PluginStart = func(name, token string) error { startedName, startedToken = name, token; return nil }
		o.PluginStop = func(name string) { stopped = append(stopped, name) }
	})
	ctx := context.Background()
	err := ps.st.Write(ctx, func(tx *store.Tx) error {
		return tx.InstallPlugin(ctx, store.Plugin{Name: "fresh", Version: "1.0.0", Kind: "first_party", Path: "/x", Scopes: []string{plugin.ScopeEventsRead}})
	})
	if err != nil {
		t.Fatal(err)
	}
	status, body := ps.do(http.MethodPost, "/api/v1/plugins/fresh/enable", "", struct{}{})
	if status != http.StatusOK {
		t.Fatalf("enable = %d %s", status, body)
	}
	if startedName != "fresh" || startedToken == "" {
		t.Fatalf("start hook got (%q, %d bytes of token)", startedName, len(startedToken))
	}
	// The stored hash is the minted token's; the response never carries it.
	p, err := ps.st.GetPlugin(ctx, "fresh")
	if err != nil {
		t.Fatal(err)
	}
	if !p.Enabled || p.TokenHash != plugin.HashToken(startedToken) {
		t.Errorf("row enabled=%v hash-match=%v, want true/true", p.Enabled, p.TokenHash == plugin.HashToken(startedToken))
	}
	if bytes.Contains(body, []byte(startedToken)) {
		t.Error("enable response leaked the plain token")
	}
	// The minted token authenticates as the plugin.
	if status, _ := ps.do(http.MethodGet, "/api/v1/journal", startedToken, nil); status != http.StatusOK {
		t.Error("minted token did not authenticate")
	}
	// Disable stops the process and revokes the token.
	if status, _ := ps.do(http.MethodPost, "/api/v1/plugins/fresh/disable", "", struct{}{}); status != http.StatusNoContent {
		t.Fatal("disable failed")
	}
	if len(stopped) == 0 || stopped[0] != "fresh" {
		t.Errorf("stop hook calls = %v, want [fresh]", stopped)
	}
	if status, _ := ps.do(http.MethodGet, "/api/v1/journal", startedToken, nil); status != http.StatusOK {
		// A revoked token is simply unknown: it falls back to today's
		// no-plugin behavior, which for GET /api/v1/journal is an open read.
		t.Error("unknown bearer on an open read should keep today's behavior")
	}
	if p, err = ps.st.GetPlugin(ctx, "fresh"); err != nil || p.Enabled || p.TokenHash != "" {
		t.Errorf("after disable: enabled=%v hash=%q (err %v), want disabled and empty", p.Enabled, p.TokenHash, err)
	}
	// Enable of a plugin that was never installed is 404.
	if status, _ := ps.do(http.MethodPost, "/api/v1/plugins/ghost/enable", "", struct{}{}); status != http.StatusNotFound {
		t.Error("enabling an uninstalled plugin should be 404")
	}
}

func TestPluginInstallEndpoint(t *testing.T) {
	t.Parallel()
	ps := newPluginServer(t, transportUnix, nil)
	dir := filepath.Join(ps.home, "plugins", "demo")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	manifest := `name = "demo"
version = "0.1.0"
command = ["./run"]
capabilities = ["events"]
scopes = ["events:read"]
`
	if err := os.WriteFile(filepath.Join(dir, "plugin.toml"), []byte(manifest), 0o600); err != nil {
		t.Fatal(err)
	}
	status, body := ps.do(http.MethodPost, "/api/v1/plugins/install", "", map[string]string{"name": "demo", "kind": "first_party"})
	if status != http.StatusOK {
		t.Fatalf("install = %d %s", status, body)
	}
	var row pluginInfo
	if err := json.Unmarshal(body, &row); err != nil {
		t.Fatal(err)
	}
	if row.Name != "demo" || row.Kind != "first_party" || row.Enabled || row.Version != "0.1.0" {
		t.Errorf("installed row = %+v", row)
	}
	// GET /api/v1/plugins lists it merged with (empty) health.
	status, body = ps.do(http.MethodGet, "/api/v1/plugins", "", nil)
	if status != http.StatusOK || !bytes.Contains(body, []byte(`"demo"`)) {
		t.Errorf("list = %d %s", status, body)
	}
	// A plugin directory without a manifest is the client's mistake.
	if status, _ := ps.do(http.MethodPost, "/api/v1/plugins/install", "", map[string]string{"name": "nosuch"}); status != http.StatusBadRequest {
		t.Errorf("install of a missing plugin = %d, want 400", status)
	}
	// Uninstall removes the row.
	if status, _ := ps.do(http.MethodDelete, "/api/v1/plugins/demo", "", nil); status != http.StatusNoContent {
		t.Error("uninstall failed")
	}
	if status, _ := ps.do(http.MethodDelete, "/api/v1/plugins/demo", "", nil); status != http.StatusNotFound {
		t.Error("second uninstall should be 404")
	}
}

// sseEvent is one parsed frame of the journal stream.
type ssePluginEvent struct {
	event, id, data string
}

func readSSEEvents(t *testing.T, r *bufio.Reader, n int) []ssePluginEvent {
	t.Helper()
	var out []ssePluginEvent
	cur := ssePluginEvent{}
	got := false
	for len(out) < n {
		line, err := r.ReadString('\n')
		if err != nil {
			t.Fatalf("stream ended after %d events: %v", len(out), err)
		}
		line = strings.TrimRight(line, "\r\n")
		if line == "" {
			if got {
				out = append(out, cur)
				cur, got = ssePluginEvent{}, false
			}
			continue
		}
		field, value, _ := strings.Cut(line, ":")
		value = strings.TrimPrefix(value, " ")
		switch field {
		case "event":
			cur.event, got = value, true
		case "id":
			cur.id, got = value, true
		case "data":
			cur.data, got = value, true
		case "retry":
			cur.event, cur.data, got = "retry", value, true
		}
	}
	return out
}

// An out-of-tree plugin — one under a configured plugin_dir, not <home>/plugins
// — installs by name and its stored path points at the configured directory, so
// the daemon starts it from there (DESIGN.md §17).
func TestPluginInstallFromConfiguredRoot(t *testing.T) {
	t.Parallel()
	extRoot := t.TempDir()
	ps := newPluginServer(t, transportUnix, func(o *ServerOptions) {
		o.PluginRoots = []string{filepath.Join(o.Home, "plugins"), extRoot}
	})
	dir := filepath.Join(extRoot, "ext")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	manifest := `name = "ext"
version = "0.9.0"
command = ["./run"]
capabilities = ["events"]
scopes = ["events:read"]
`
	if err := os.WriteFile(filepath.Join(dir, "plugin.toml"), []byte(manifest), 0o600); err != nil {
		t.Fatal(err)
	}
	status, body := ps.do(http.MethodPost, "/api/v1/plugins/install", "", map[string]string{"name": "ext", "kind": "third_party"})
	if status != http.StatusOK {
		t.Fatalf("install = %d %s", status, body)
	}
	p, err := ps.st.GetPlugin(context.Background(), "ext")
	if err != nil {
		t.Fatal(err)
	}
	if p.Path != dir {
		t.Errorf("stored path = %q, want the configured root %q", p.Path, dir)
	}
	if p.Version != "0.9.0" || p.Kind != "third_party" {
		t.Errorf("installed row = %+v", p)
	}
}

func TestJournalSSECatchUpSinceAndAck(t *testing.T) {
	t.Parallel()
	ps := newPluginServer(t, transportUnix, nil)
	tok := ps.installEnabled("eventer", []string{plugin.ScopeEventsRead})
	ctx := context.Background()
	for range 3 {
		if err := ps.st.Write(ctx, func(tx *store.Tx) error {
			return tx.Journal(ctx, "test.ping", store.EntityDaemon, "daemon", map[string]any{"n": 1})
		}); err != nil {
			t.Fatal(err)
		}
	}
	entries, err := ps.st.JournalSince(ctx, 0, 100)
	if err != nil {
		t.Fatal(err)
	}
	total := len(entries)

	stream := func(path string) (*http.Response, *bufio.Reader) {
		req, err := http.NewRequest(http.MethodGet, ps.http.URL+path, nil)
		if err != nil {
			t.Fatal(err)
		}
		req.Header.Set("Authorization", "Bearer "+tok)
		resp, err := ps.http.Client().Do(req)
		if err != nil {
			t.Fatal(err)
		}
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("stream status = %d", resp.StatusCode)
		}
		if ct := resp.Header.Get("Content-Type"); ct != "text/event-stream" {
			t.Fatalf("content-type = %q", ct)
		}
		return resp, bufio.NewReader(resp.Body)
	}

	// Catch-up from zero replays everything, ids ascending.
	resp, br := stream("/api/v1/journal?follow=1&since=0")
	events := readSSEEvents(t, br, total)
	if err := resp.Body.Close(); err != nil {
		t.Fatal(err)
	}
	lastID := ""
	for i, e := range events {
		if e.event != "journal" {
			t.Fatalf("event[%d] = %q, want journal", i, e.event)
		}
		if e.id <= lastID && len(e.id) == len(lastID) {
			t.Fatalf("ids not ascending: %q after %q", e.id, lastID)
		}
		var entry store.JournalEntry
		if err := json.Unmarshal([]byte(e.data), &entry); err != nil {
			t.Fatalf("event[%d] data %q: %v", i, e.data, err)
		}
		lastID = e.id
	}

	// since=N resumes past what was acked: ack the second-to-last id, then
	// stream from the acked cursor and expect exactly the later entries.
	ackAt := entries[total-2].ID
	if status, _ := ps.do(http.MethodPost, "/api/v1/plugins/eventer/ack", tok, map[string]int64{"cursor": ackAt}); status != http.StatusNoContent {
		t.Fatal("ack failed")
	}
	p, err := ps.st.GetPlugin(ctx, "eventer")
	if err != nil {
		t.Fatal(err)
	}
	if p.Cursor != ackAt {
		t.Fatalf("persisted cursor = %d, want %d", p.Cursor, ackAt)
	}
	resp, br = stream("/api/v1/journal?follow=1&since=" + jsonNumber(ackAt))
	tail := readSSEEvents(t, br, 1)
	if err := resp.Body.Close(); err != nil {
		t.Fatal(err)
	}
	var entry store.JournalEntry
	if err := json.Unmarshal([]byte(tail[0].data), &entry); err != nil {
		t.Fatal(err)
	}
	if entry.ID <= ackAt {
		t.Errorf("resumed entry id = %d, want > %d", entry.ID, ackAt)
	}

	// A draining daemon closes the stream with a retry hint after catch-up.
	ps.srv.SetDraining(true)
	resp, br = stream("/api/v1/journal?follow=1&since=" + jsonNumber(entries[total-1].ID))
	hint := readSSEEvents(t, br, 1)
	if err := resp.Body.Close(); err != nil {
		t.Fatal(err)
	}
	if hint[0].event != "retry" || hint[0].data != "2000" {
		t.Errorf("drain close = %+v, want retry: 2000", hint[0])
	}
	ps.srv.SetDraining(false)

	// Without follow (and without the SSE Accept) the JSON list survives.
	status, body := ps.do(http.MethodGet, "/api/v1/journal", tok, nil)
	if status != http.StatusOK || !json.Valid(body) || !bytes.HasPrefix(bytes.TrimSpace(body), []byte("[")) {
		t.Errorf("plain journal = %d %s, want a JSON list", status, body)
	}
}

func jsonNumber(n int64) string {
	return strconv.FormatInt(n, 10)
}

func TestAnnotateExternalRefs(t *testing.T) {
	t.Parallel()
	ps := newPluginServer(t, transportUnix, nil)
	tokAnnotate := ps.installEnabled("linker", []string{plugin.ScopeAnnotateWrite})
	tokEvents := ps.installEnabled("eventer", []string{plugin.ScopeEventsRead})
	ctx := context.Background()
	w := &store.Work{RoutineName: "manual", Generation: 1, Title: "t", Trigger: model.TriggerManual,
		Snapshot: []byte(`{}`), Priority: 100, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto}
	if err := ps.st.Write(ctx, func(tx *store.Tx) error {
		_, err := tx.CreateWork(ctx, w, []string{"repo1"}, nil)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	refs := map[string]any{"refs": []map[string]string{{"kind": "issue", "id": "42", "url": "https://x/42", "label": "issue 42"}}}
	status, body := ps.do(http.MethodPost, "/api/v1/work/"+w.ID+"/external-refs", tokAnnotate, refs)
	if status != http.StatusNoContent {
		t.Fatalf("annotate = %d %s", status, body)
	}
	got, err := ps.st.GetWork(ctx, w.ID)
	if err != nil {
		t.Fatal(err)
	}
	var stored []store.ExternalRef
	if err := json.Unmarshal(got.ExternalRefs, &stored); err != nil {
		t.Fatalf("external_refs %s: %v", got.ExternalRefs, err)
	}
	if len(stored) != 1 || stored[0].Plugin != "linker" || stored[0].ID != "42" {
		t.Errorf("stored refs = %+v (the caller's name is stamped)", stored)
	}
	// Re-annotating the same ref is a no-op (dedupe by plugin/kind/id).
	if status, _ := ps.do(http.MethodPost, "/api/v1/work/"+w.ID+"/external-refs", tokAnnotate, refs); status != http.StatusNoContent {
		t.Fatal("re-annotate failed")
	}
	got, err = ps.st.GetWork(ctx, w.ID)
	if err != nil {
		t.Fatal(err)
	}
	stored = nil
	if err := json.Unmarshal(got.ExternalRefs, &stored); err != nil {
		t.Fatal(err)
	}
	if len(stored) != 1 {
		t.Errorf("refs after duplicate annotate = %d, want 1", len(stored))
	}
	// A plugin cannot annotate in another plugin's name.
	bad := map[string]any{"refs": []map[string]string{{"plugin": "imposter", "kind": "x", "id": "1"}}}
	if status, _ := ps.do(http.MethodPost, "/api/v1/work/"+w.ID+"/external-refs", tokAnnotate, bad); status != http.StatusBadRequest {
		t.Error("mismatched ref plugin should be 400")
	}
	// annotate:write is required.
	if status, _ := ps.do(http.MethodPost, "/api/v1/work/"+w.ID+"/external-refs", tokEvents, refs); status != http.StatusForbidden {
		t.Error("annotate without annotate:write should be 403")
	}
	// The journal recorded the annotation.
	found := false
	for _, k := range ps.journalKinds() {
		if k == "work.annotated" {
			found = true
		}
	}
	if !found {
		t.Error("no work.annotated journal row")
	}
}
