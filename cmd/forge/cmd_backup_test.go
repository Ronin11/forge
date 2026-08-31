package main

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/controlplane"
	"forge/internal/store"
)

// restoreContext is a cmdContext whose home the test controls.
func restoreContext(home string) (*cmdContext, *strings.Builder, *strings.Builder) {
	var out, errOut strings.Builder
	c := &cmdContext{stdout: &out, stderr: &errOut, getenv: func(string) string { return "" }, forgeHome: home, userHome: home, now: time.Now}
	return c, &out, &errOut
}

// makeArchive builds a real backup archive from a seeded store, returning it
// and the number of plugins it holds.
func makeArchive(t *testing.T) string {
	t.Helper()
	ctx := context.Background()
	seedHome := t.TempDir()
	st, err := store.Open(ctx, filepath.Join(seedHome, controlplane.DBFile), store.Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	}()
	if err := st.Write(ctx, func(tx *store.Tx) error {
		if err := tx.EnsureProject(ctx, "default"); err != nil {
			return err
		}
		return tx.InstallPlugin(ctx, store.Plugin{Name: "p", Version: "1", Kind: "first_party", Path: "/x", Scopes: []string{"events:read"}})
	}); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(seedHome, "config.toml"), []byte("# config\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	archive, err := controlplane.WriteBackupArchive(ctx, st, controlplane.BackupInputs{Home: seedHome, OutDir: t.TempDir()})
	if err != nil {
		t.Fatal(err)
	}
	return archive
}

func TestRestoreRoundTrip(t *testing.T) {
	archive := makeArchive(t)
	home := filepath.Join(t.TempDir(), "fresh-home") // does not exist yet
	c, out, errOut := restoreContext(home)
	if code := runRestore(context.Background(), c, []string{archive}); code != 0 {
		t.Fatalf("restore = %d, stderr %s", code, errOut.String())
	}
	if !strings.Contains(out.String(), "daemon start") {
		t.Errorf("restore output lacks next steps: %q", out.String())
	}
	st, err := store.Open(context.Background(), filepath.Join(home, controlplane.DBFile), store.Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	}()
	plugins, err := st.Plugins(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(plugins) != 1 || plugins[0].Name != "p" {
		t.Errorf("restored plugins = %+v", plugins)
	}
}

func TestRestoreRefusesNonEmptyHome(t *testing.T) {
	archive := makeArchive(t)
	home := t.TempDir()
	if err := os.WriteFile(filepath.Join(home, "token"), []byte("occupied"), 0o600); err != nil {
		t.Fatal(err)
	}
	c, _, errOut := restoreContext(home)
	if code := runRestore(context.Background(), c, []string{archive}); code != 1 {
		t.Fatalf("restore into a non-empty home = %d", code)
	}
	if !strings.Contains(errOut.String(), "not empty") {
		t.Errorf("stderr = %q", errOut.String())
	}
	// The occupied home is untouched.
	if _, err := os.Stat(filepath.Join(home, controlplane.DBFile)); !os.IsNotExist(err) {
		t.Errorf("restore into a refused home still wrote the db: %v", err)
	}
}

func TestRestoreUsage(t *testing.T) {
	c, _, _ := restoreContext(t.TempDir())
	if code := runRestore(context.Background(), c, nil); code != 2 {
		t.Errorf("restore without an archive = %d, want 2", code)
	}
}
