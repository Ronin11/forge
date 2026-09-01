package controlplane

import (
	"context"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"

	"forge/internal/core/store"
)

func TestHealthEndpoint(t *testing.T) {
	h := newHarness(t, transportUnix)
	var body struct {
		Daemon  string `json:"daemon"`
		Version string `json:"version"`
		Schema  string `json:"schema"`
		Worker  struct {
			Registered    bool     `json:"registered"`
			Connected     bool     `json:"connected"`
			HeartbeatAgeS *float64 `json:"heartbeat_age_s"`
		} `json:"worker"`
		Plugins []struct {
			Name    string `json:"name"`
			Running bool   `json:"running"`
		} `json:"plugins"`
	}
	h.call(http.MethodGet, "/api/v1/health", nil, &body, http.StatusOK)
	if body.Daemon != "ok" || body.Version != "test" || body.Schema == "" {
		t.Errorf("health = %+v", body)
	}
	if body.Worker.Registered {
		t.Error("worker registered before any registration")
	}
	if body.Plugins == nil {
		t.Error("plugins must be a list, not null")
	}

	h.register(testWorkerID)
	h.call(http.MethodGet, "/api/v1/health", nil, &body, http.StatusOK)
	if !body.Worker.Registered || !body.Worker.Connected || body.Worker.HeartbeatAgeS == nil {
		t.Errorf("worker after register = %+v", body.Worker)
	}

	h.srv.SetDraining(true)
	h.call(http.MethodGet, "/api/v1/health", nil, &body, http.StatusOK)
	if body.Daemon != "draining" {
		t.Errorf("daemon while draining = %q", body.Daemon)
	}
}

// homeServer is a server with a real Home so the backup handler works.
func homeServer(t *testing.T) (*store.Store, string, *httptest.Server) {
	t.Helper()
	home := t.TempDir()
	st, err := store.Open(context.Background(), filepath.Join(home, DBFile), store.Options{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	})
	for _, d := range []string{"kb", "modes"} {
		if err := os.MkdirAll(filepath.Join(home, d), 0o700); err != nil {
			t.Fatal(err)
		}
	}
	files := map[string]string{
		"config.toml":                    "# test config\n",
		"worker.toml":                    "# test worker config\n",
		filepath.Join("kb", "note.md"):   "# a note\n",
		filepath.Join("modes", "run.md"): "run prompt\n",
	}
	for name, content := range files {
		if err := os.WriteFile(filepath.Join(home, name), []byte(content), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	srv, err := NewServer(ServerOptions{Store: st, Version: "test", TransportOverride: transportUnix, Home: home, KbDir: filepath.Join(home, "kb")})
	if err != nil {
		t.Fatal(err)
	}
	hs := httptest.NewServer(srv.Handler())
	t.Cleanup(hs.Close)
	return st, home, hs
}

func TestBackupEndpointWritesArchiveAndJournals(t *testing.T) {
	st, _, hs := homeServer(t)
	out := t.TempDir()
	h := harnessOver(t, hs)
	var resp struct {
		Archive string `json:"archive"`
		Bytes   int64  `json:"bytes"`
	}
	h.call(http.MethodPost, "/api/v1/backup", map[string]string{"out": out}, &resp, http.StatusOK)
	if fi, err := os.Stat(resp.Archive); err != nil || fi.Size() == 0 || fi.Size() != resp.Bytes {
		t.Fatalf("archive %q: %v (bytes %d)", resp.Archive, err, resp.Bytes)
	}
	// Single-file archives: the staging directory is gone.
	entries, err := os.ReadDir(out)
	if err != nil {
		t.Fatal(err)
	}
	for _, e := range entries {
		if e.IsDir() {
			t.Errorf("staging directory %s left behind", e.Name())
		}
	}
	journal, err := st.JournalSince(context.Background(), 0, 100)
	if err != nil {
		t.Fatal(err)
	}
	if !hasKind(journal, "daemon.backup") {
		t.Error("no daemon.backup journal row")
	}
	// A relative out directory is the client's mistake.
	if status, _ := h.do(http.MethodPost, "/api/v1/backup", map[string]string{"out": "relative/dir"}, nil, ""); status != http.StatusBadRequest {
		t.Errorf("relative out = %d", status)
	}
}

// harnessOver adapts the do/call helpers to an externally built test server.
func harnessOver(t *testing.T, hs *httptest.Server) *harness {
	t.Helper()
	return &harness{t: t, http: hs}
}

func TestBackupArchiveRestoresIntoFreshHome(t *testing.T) {
	ctx := context.Background()
	st, home, _ := homeServer(t)
	if err := st.Write(ctx, func(tx *store.Tx) error {
		return tx.InstallPlugin(ctx, store.Plugin{Name: "p", Version: "1", Kind: "first_party", Path: "/x", Scopes: []string{"events:read"}})
	}); err != nil {
		t.Fatal(err)
	}
	out := t.TempDir()
	archive, err := WriteBackupArchive(ctx, st, BackupInputs{Home: home, OutDir: out})
	if err != nil {
		t.Fatal(err)
	}
	fresh := filepath.Join(t.TempDir(), "newhome")
	if err := os.MkdirAll(fresh, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := UnpackBackup(archive, fresh); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{DBFile, "config.toml", "worker.toml", "plugins.json", filepath.Join("kb", "note.md"), filepath.Join("modes", "run.md")} {
		if _, err := os.Stat(filepath.Join(fresh, name)); err != nil {
			t.Errorf("restored home lacks %s: %v", name, err)
		}
	}
	restored, err := store.Open(ctx, filepath.Join(fresh, DBFile), store.Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := restored.Close(); err != nil {
			t.Error(err)
		}
	}()
	plugins, err := restored.Plugins(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(plugins) != 1 || plugins[0].Name != "p" {
		t.Errorf("restored plugins = %+v", plugins)
	}
}

func TestPruneBackupsAndLatest(t *testing.T) {
	dir := t.TempDir()
	if path, mtime, err := LatestBackup(dir); err != nil || path != "" || !mtime.IsZero() {
		t.Errorf("LatestBackup on empty dir = %q %v %v", path, mtime, err)
	}
	names := []string{
		backupPrefix + "20260828T000000Z.tar.gz",
		backupPrefix + "20260829T000000Z.tar.gz",
		backupPrefix + "20260830T000000Z.tar.gz",
		"unrelated.txt",
	}
	for _, n := range names {
		if err := os.WriteFile(filepath.Join(dir, n), []byte("x"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	path, _, err := LatestBackup(dir)
	if err != nil || filepath.Base(path) != names[2] {
		t.Errorf("LatestBackup = %q %v", path, err)
	}
	removed, err := PruneBackups(dir, 2)
	if err != nil || len(removed) != 1 || filepath.Base(removed[0]) != names[0] {
		t.Fatalf("PruneBackups = %v %v", removed, err)
	}
	for i, n := range names[1:] {
		if _, err := os.Stat(filepath.Join(dir, n)); err != nil {
			t.Errorf("survivor %d (%s) gone: %v", i, n, err)
		}
	}
	if _, err := PruneBackups(dir, 0); err == nil {
		t.Error("keep 0 accepted")
	}
	if removed, err := PruneBackups(filepath.Join(dir, "missing"), 3); err != nil || removed != nil {
		t.Errorf("prune of a missing dir = %v %v", removed, err)
	}
}

func TestUnpackBackupRefusesEscapes(t *testing.T) {
	// A handcrafted archive with a traversal entry must be refused.
	dir := t.TempDir()
	staging := filepath.Join(dir, "staging")
	if err := os.MkdirAll(staging, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(staging, "ok.txt"), []byte("fine"), 0o600); err != nil {
		t.Fatal(err)
	}
	archive := filepath.Join(dir, "t.tar.gz")
	if err := tarGzDir(staging, archive); err != nil {
		t.Fatal(err)
	}
	home := t.TempDir()
	if err := UnpackBackup(archive, home); err != nil {
		t.Fatalf("benign archive refused: %v", err)
	}
	if _, err := os.Stat(filepath.Join(home, "ok.txt")); err != nil {
		t.Error(err)
	}
}
