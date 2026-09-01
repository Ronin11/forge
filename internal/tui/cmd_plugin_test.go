package tui

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"forge/internal/core/daemon"
	"forge/internal/core/protocol"
)

// stubDaemon serves mux on the home's unix socket so the CLI client connects
// as if a daemon were running.
func stubDaemon(t *testing.T, home string, mux *http.ServeMux) {
	t.Helper()
	if err := os.MkdirAll(home, 0o700); err != nil {
		t.Fatal(err)
	}
	mux.HandleFunc("GET /api/v1/handshake", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		if err := json.NewEncoder(w).Encode(protocol.Handshake{Version: "dev", SchemaVersion: "test", State: "running", PID: 1}); err != nil {
			t.Error(err)
		}
	})
	l, err := net.Listen("unix", filepath.Join(home, daemon.SocketFile))
	if err != nil {
		t.Fatal(err)
	}
	srv := &http.Server{Handler: mux}
	done := make(chan struct{})
	go func() {
		defer close(done)
		if err := srv.Serve(l); err != nil && !errors.Is(err, http.ErrServerClosed) {
			t.Error(err)
		}
	}()
	t.Cleanup(func() {
		if err := srv.Close(); err != nil {
			t.Error(err)
		}
		<-done
	})
}

func pluginCmdContext(t *testing.T, home, stdin string) (*Context, *strings.Builder, *strings.Builder) {
	t.Helper()
	var out, errOut strings.Builder
	c := &Context{
		Stdin: strings.NewReader(stdin), Stdout: &out, Stderr: &errOut,
		Getenv: func(string) string { return "" }, ForgeHome: home, UserHome: t.TempDir(), Now: time.Now, Version: "dev",
	}
	return c, &out, &errOut
}

func writeRow(t *testing.T, w http.ResponseWriter, v any) {
	t.Helper()
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(v); err != nil {
		t.Error(err)
	}
}

func TestPluginEnablePromptsForScopes(t *testing.T) {
	home := filepath.Join(t.TempDir(), "forge-home")
	mux := http.NewServeMux()
	var enabled atomic.Bool
	row := pluginRow{Name: "testp", Version: "1.0.0", Kind: "first_party", Scopes: []string{"events:read", "tools:provide"}}
	mux.HandleFunc("GET /api/v1/plugins/testp", func(w http.ResponseWriter, _ *http.Request) { writeRow(t, w, row) })
	mux.HandleFunc("POST /api/v1/plugins/testp/enable", func(w http.ResponseWriter, _ *http.Request) {
		enabled.Store(true)
		r := row
		r.Enabled = true
		writeRow(t, w, r)
	})
	stubDaemon(t, home, mux)

	// Declined: the prompt answers "n" and nothing is enabled.
	c, out, _ := pluginCmdContext(t, home, "n\n")
	if code := RunPlugin(context.Background(), c, []string{"enable", "testp"}); code != 1 {
		t.Fatalf("declined enable exit = %d, want 1\n%s", code, out.String())
	}
	if enabled.Load() {
		t.Fatal("declined enable still POSTed")
	}
	if !strings.Contains(out.String(), "enabling testp grants: events:read, tools:provide") {
		t.Errorf("scopes not shown:\n%s", out.String())
	}

	// Approved with "y".
	c, out, _ = pluginCmdContext(t, home, "y\n")
	if code := RunPlugin(context.Background(), c, []string{"enable", "testp"}); code != 0 {
		t.Fatalf("enable exit = %d\n%s", code, out.String())
	}
	if !enabled.Load() {
		t.Fatal("approved enable never POSTed")
	}
	if !strings.Contains(out.String(), "plugin testp enabled") {
		t.Errorf("no confirmation:\n%s", out.String())
	}
	if !strings.Contains(out.String(), "daemon restart") {
		t.Errorf("tools:provide should print the restart note:\n%s", out.String())
	}

	// --yes skips the prompt entirely (empty stdin).
	enabled.Store(false)
	c, out, _ = pluginCmdContext(t, home, "")
	if code := RunPlugin(context.Background(), c, []string{"enable", "testp", "--yes"}); code != 0 {
		t.Fatalf("--yes enable exit = %d\n%s", code, out.String())
	}
	if !enabled.Load() {
		t.Fatal("--yes enable never POSTed")
	}
}

func TestPluginStatusTable(t *testing.T) {
	home := filepath.Join(t.TempDir(), "forge-home")
	mux := http.NewServeMux()
	rows := []pluginRow{
		{Name: "alpha", Version: "1.0.0", Kind: "first_party", Enabled: true, Running: true, PID: 42, Restarts: 2, Cursor: 17},
		{Name: "beta", Version: "0.2.0", Kind: "third_party", LastExit: "exit status 1"},
	}
	mux.HandleFunc("GET /api/v1/plugins", func(w http.ResponseWriter, _ *http.Request) { writeRow(t, w, rows) })
	stubDaemon(t, home, mux)
	c, out, _ := pluginCmdContext(t, home, "")
	if code := RunPlugin(context.Background(), c, []string{"status"}); code != 0 {
		t.Fatalf("status exit = %d\n%s", code, out.String())
	}
	for _, want := range []string{"alpha", "42", "beta", "exit status 1"} {
		if !strings.Contains(out.String(), want) {
			t.Errorf("status table missing %q:\n%s", want, out.String())
		}
	}
}

func TestPluginUninstallRemovesFiles(t *testing.T) {
	home := filepath.Join(t.TempDir(), "forge-home")
	mux := http.NewServeMux()
	var deleted atomic.Bool
	mux.HandleFunc("DELETE /api/v1/plugins/testp", func(w http.ResponseWriter, _ *http.Request) {
		deleted.Store(true)
		w.WriteHeader(http.StatusNoContent)
	})
	stubDaemon(t, home, mux)
	dir := filepath.Join(home, "plugins", "testp")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	c, out, _ := pluginCmdContext(t, home, "")
	if code := RunPlugin(context.Background(), c, []string{"uninstall", "testp"}); code != 0 {
		t.Fatalf("uninstall exit = %d\n%s", code, out.String())
	}
	if !deleted.Load() {
		t.Error("daemon DELETE never happened")
	}
	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Errorf("plugin directory survived uninstall: %v", err)
	}
}

func TestPluginLogsTail(t *testing.T) {
	home := filepath.Join(t.TempDir(), "forge-home")
	logDir := filepath.Join(home, "logs", "plugins")
	if err := os.MkdirAll(logDir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(logDir, "testp.log"), []byte("one\ntwo\nthree\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	c, out, _ := pluginCmdContext(t, home, "")
	if code := RunPlugin(context.Background(), c, []string{"logs", "testp", "-n", "2"}); code != 0 {
		t.Fatalf("logs exit = %d", code)
	}
	if got := out.String(); got != "two\nthree\n" {
		t.Errorf("logs -n 2 = %q, want the last two lines", got)
	}
}

func TestFindRepoPluginDirFrom(t *testing.T) {
	t.Parallel()
	root := t.TempDir()
	src := filepath.Join(root, "plugins", "foo")
	if err := os.MkdirAll(src, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(src, "plugin.toml"), []byte("name = \"foo\"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	start := filepath.Join(root, "cmd", "forge")
	if err := os.MkdirAll(start, 0o700); err != nil {
		t.Fatal(err)
	}
	got, err := findRepoPluginDirFrom(start, "foo")
	if err != nil {
		t.Fatal(err)
	}
	if got != src {
		t.Errorf("found %q, want %q", got, src)
	}
	if _, err := findRepoPluginDirFrom(start, "missing"); err == nil || !strings.Contains(err.Error(), "plugins/missing/plugin.toml") {
		t.Errorf("missing plugin error = %v, want a clear line naming the path", err)
	}
}

// TestPluginListDiscoversConfiguredDir proves an out-of-tree plugin — one that
// lives under a plugin_dir in config.toml, outside any Forge checkout — is
// discovered and listed as "available" by `forge plugin list`, with no change
// to the checkout (the acceptance for out-of-tree plugins, DESIGN.md §17).
func TestPluginListDiscoversConfiguredDir(t *testing.T) {
	home := filepath.Join(t.TempDir(), "forge-home")
	mux := http.NewServeMux()
	// The daemon knows of no installed plugins; the CLI adds discovered ones.
	mux.HandleFunc("GET /api/v1/plugins", func(w http.ResponseWriter, _ *http.Request) { writeRow(t, w, []pluginRow{}) })
	stubDaemon(t, home, mux)

	// A plugin living entirely outside the repo.
	outside := t.TempDir()
	pdir := filepath.Join(outside, "foo")
	if err := os.MkdirAll(pdir, 0o700); err != nil {
		t.Fatal(err)
	}
	manifest := "name = \"foo\"\nversion = \"0.3.0\"\ncommand = [\"./foo\"]\ncapabilities = [\"events\"]\nscopes = [\"events:read\"]\n"
	if err := os.WriteFile(filepath.Join(pdir, "plugin.toml"), []byte(manifest), 0o600); err != nil {
		t.Fatal(err)
	}
	// config.toml points a plugin_dir at that directory.
	cfg := "plugin_dirs = [\"" + outside + "\"]\n"
	if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte(cfg), 0o600); err != nil {
		t.Fatal(err)
	}

	c, out, _ := pluginCmdContext(t, home, "")
	if code := RunPlugin(context.Background(), c, []string{"list"}); code != 0 {
		t.Fatalf("plugin list exit = %d\n%s", code, out.String())
	}
	if !strings.Contains(out.String(), "foo") || !strings.Contains(out.String(), "available") {
		t.Errorf("out-of-tree plugin not listed as available:\n%s", out.String())
	}
}
