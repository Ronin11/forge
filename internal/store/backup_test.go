package store

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// seedBackupStore fills a store with one plugin and one proposal so a backup
// has rows worth comparing.
func seedBackupStore(t *testing.T, st *Store) {
	t.Helper()
	ctx := context.Background()
	err := st.Write(ctx, func(tx *Tx) error {
		if err := tx.EnsureProject(ctx, "default"); err != nil {
			return err
		}
		return tx.InstallPlugin(ctx, Plugin{Name: "status-file", Version: "1.0", Kind: "first_party", Path: "/x", Scopes: []string{"events:read"}})
	})
	if err != nil {
		t.Fatal(err)
	}
}

func TestBackupIntoRoundTrip(t *testing.T) {
	ctx := context.Background()
	st := openTest(t)
	seedBackupStore(t, st)
	p := mkProposal(t, st, "process", "routine:x")

	dst := filepath.Join(t.TempDir(), "nested", "backup.sqlite3")
	if err := st.BackupInto(ctx, dst); err != nil {
		t.Fatal(err)
	}
	restored, err := Open(ctx, dst, Options{Clock: func() time.Time { return time.Date(2026, 8, 30, 13, 0, 0, 0, time.UTC) }})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := restored.Close(); err != nil {
			t.Error(err)
		}
	})
	if restored.SchemaVersion() != st.SchemaVersion() {
		t.Errorf("schema = %s, want %s", restored.SchemaVersion(), st.SchemaVersion())
	}
	plugins, err := restored.Plugins(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(plugins) != 1 || plugins[0].Name != "status-file" {
		t.Errorf("plugins = %+v", plugins)
	}
	got, err := restored.GetProposal(ctx, p.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.Target != "routine:x" {
		t.Errorf("proposal target = %q", got.Target)
	}
	// The original keeps working after the backup.
	if _, err := st.Plugins(ctx); err != nil {
		t.Errorf("source store after backup: %v", err)
	}
}

func TestBackupIntoRefusesExistingDestination(t *testing.T) {
	ctx := context.Background()
	st := openTest(t)
	dst := filepath.Join(t.TempDir(), "backup.sqlite3")
	if err := os.WriteFile(dst, []byte("already here"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := st.BackupInto(ctx, dst); err == nil {
		t.Fatal("BackupInto overwrote an existing file")
	}
	if err := st.BackupInto(ctx, ""); err == nil {
		t.Fatal("BackupInto accepted an empty path")
	}
}

// TestPreMigrationSnapshot proves the hook migrate calls before a pending
// migration on an existing database yields an openable copy. The hook cannot
// fire for real in tests — that needs a migration this binary knows but the
// database does not — so the helper is pinned directly.
func TestPreMigrationSnapshot(t *testing.T) {
	ctx := context.Background()
	st := openTest(t)
	seedBackupStore(t, st)
	if err := st.preMigrationSnapshot(ctx, "01TESTMIGRATION"); err != nil {
		t.Fatal(err)
	}
	path := st.Path() + ".pre-01TESTMIGRATION"
	restored, err := Open(ctx, path, Options{})
	if err != nil {
		t.Fatalf("snapshot does not open: %v", err)
	}
	if err := restored.Close(); err != nil {
		t.Error(err)
	}
	// A second call replaces the stale snapshot instead of failing.
	if err := st.preMigrationSnapshot(ctx, "01TESTMIGRATION"); err != nil {
		t.Fatal(err)
	}
}
