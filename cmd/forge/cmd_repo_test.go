package main

import (
	"context"
	"net/http"
	"path/filepath"
	"strings"
	"testing"
)

// runRepo against a stub daemon exercises the list/show/pause/resume paths and
// their table output, mirroring the plugin CLI harness.
func TestRepoCLI(t *testing.T) {
	home := filepath.Join(t.TempDir(), "forge-home")
	mux := http.NewServeMux()
	paused := false
	mux.HandleFunc("GET /api/v1/repositories", func(w http.ResponseWriter, _ *http.Request) {
		writeRow(t, w, []map[string]any{{
			"name": "demo", "project": "default", "path": "/tmp/demo",
			"origin_identity": "github.com/x/demo", "paused": paused, "state": "idle",
		}})
	})
	mux.HandleFunc("GET /api/v1/repositories/demo", func(w http.ResponseWriter, _ *http.Request) {
		writeRow(t, w, map[string]any{
			"repository": map[string]any{"name": "demo", "path": "/tmp/demo", "origin_identity": "github.com/x/demo", "paused": paused},
			"state":      "idle", "paused": paused, "retained_count": 2,
			"checks": []string{"lint", "test"}, "app_url": "https://localhost:3000",
			"running": []any{}, "recent": []any{},
		})
	})
	mux.HandleFunc("POST /api/v1/repositories/demo/pause", func(w http.ResponseWriter, _ *http.Request) {
		paused = true
		writeRow(t, w, map[string]any{"name": "demo", "paused": true})
	})
	mux.HandleFunc("POST /api/v1/repositories/demo/resume", func(w http.ResponseWriter, _ *http.Request) {
		paused = false
		writeRow(t, w, map[string]any{"name": "demo", "paused": false})
	})
	mux.HandleFunc("POST /api/v1/repositories/demo/cancel-running", func(w http.ResponseWriter, _ *http.Request) {
		writeRow(t, w, map[string]any{"cancelled": 3})
	})
	stubDaemon(t, home, mux)

	ctx := context.Background()

	c, out, _ := pluginCmdContext(t, home, "")
	if code := runRepo(ctx, c, []string{"list"}); code != 0 {
		t.Fatalf("repo list exit = %d\n%s", code, out.String())
	}
	if s := out.String(); !strings.Contains(s, "demo") || !strings.Contains(s, "idle") || !strings.Contains(s, "github.com/x/demo") {
		t.Errorf("repo list output = %q", s)
	}

	c, out, _ = pluginCmdContext(t, home, "")
	if code := runRepo(ctx, c, []string{"show", "demo"}); code != 0 {
		t.Fatalf("repo show exit = %d\n%s", code, out.String())
	}
	if s := out.String(); !strings.Contains(s, "repository demo") || !strings.Contains(s, "retained worktrees: 2") || !strings.Contains(s, "lint, test") || !strings.Contains(s, "https://localhost:3000") {
		t.Errorf("repo show output = %q", s)
	}

	c, out, _ = pluginCmdContext(t, home, "")
	if code := runRepo(ctx, c, []string{"pause", "demo"}); code != 0 || !strings.Contains(out.String(), "paused true") {
		t.Fatalf("repo pause = %d %q", code, out.String())
	}
	if !paused {
		t.Error("pause did not reach the daemon")
	}

	c, out, _ = pluginCmdContext(t, home, "")
	if code := runRepo(ctx, c, []string{"resume", "demo"}); code != 0 || !strings.Contains(out.String(), "paused false") {
		t.Fatalf("repo resume = %d %q", code, out.String())
	}

	c, out, _ = pluginCmdContext(t, home, "")
	if code := runRepo(ctx, c, []string{"cancel", "demo"}); code != 0 || !strings.Contains(out.String(), "cancelled 3 running") {
		t.Fatalf("repo cancel = %d %q", code, out.String())
	}
}
