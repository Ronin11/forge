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

func TestCaptureLastKnownGood(t *testing.T) {
	ctx := context.Background()
	home := t.TempDir()
	st, err := store.Open(ctx, filepath.Join(home, controlplane.DBFile), store.Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	}()
	exe := filepath.Join(t.TempDir(), "forge")
	if err := os.WriteFile(exe, []byte("#!/bin/sh\necho v1\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := captureLastKnownGood(ctx, st, home, exe); err != nil {
		t.Fatal(err)
	}
	bin, err := os.ReadFile(filepath.Join(home, prevDirName, prevBinName))
	if err != nil || !strings.Contains(string(bin), "echo v1") {
		t.Errorf("forge-bin-good = %q, %v", bin, err)
	}
	snap, err := store.Open(ctx, filepath.Join(home, prevDirName, prevDBName), store.Options{})
	if err != nil {
		t.Fatalf("db-good does not open: %v", err)
	}
	if err := snap.Close(); err != nil {
		t.Error(err)
	}
	// A second healthy start refreshes both without error.
	if err := os.WriteFile(exe, []byte("#!/bin/sh\necho v2\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := captureLastKnownGood(ctx, st, home, exe); err != nil {
		t.Fatal(err)
	}
	bin, err = os.ReadFile(filepath.Join(home, prevDirName, prevBinName))
	if err != nil || !strings.Contains(string(bin), "echo v2") {
		t.Errorf("refreshed forge-bin-good = %q, %v", bin, err)
	}
}

func rollbackContext(home string) (*cmdContext, *strings.Builder, *strings.Builder) {
	var out, errOut strings.Builder
	c := &cmdContext{stdout: &out, stderr: &errOut, getenv: func(string) string { return "" }, forgeHome: home, userHome: home, now: time.Now}
	return c, &out, &errOut
}

func TestRollbackRefusesRunningDaemon(t *testing.T) {
	home := t.TempDir()
	lock, err := controlplane.TryLock(home)
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := lock.Release(); err != nil {
			t.Error(err)
		}
	}()
	c, _, errOut := rollbackContext(home)
	if code := runDaemonRollback(context.Background(), c, nil); code != 1 {
		t.Fatalf("rollback with the lock held = %d", code)
	}
	if !strings.Contains(errOut.String(), "running") {
		t.Errorf("stderr = %q", errOut.String())
	}
}

func TestRollbackRefusesWithoutSnapshot(t *testing.T) {
	c, _, errOut := rollbackContext(t.TempDir())
	if code := runDaemonRollback(context.Background(), c, nil); code != 1 {
		t.Fatalf("rollback without prev/db-good = %d", code)
	}
	if !strings.Contains(errOut.String(), "last-known-good") {
		t.Errorf("stderr = %q", errOut.String())
	}
}

func TestRollbackRestoresDatabase(t *testing.T) {
	ctx := context.Background()
	home := t.TempDir()
	// The last known good: a real store snapshotted into prev/db-good.
	seed, err := store.Open(ctx, filepath.Join(t.TempDir(), "seed.sqlite3"), store.Options{})
	if err != nil {
		t.Fatal(err)
	}
	if err := seed.Write(ctx, func(tx *store.Tx) error { return tx.EnsureProject(ctx, "default") }); err != nil {
		t.Fatal(err)
	}
	if err := seed.BackupInto(ctx, filepath.Join(home, prevDirName, prevDBName)); err != nil {
		t.Fatal(err)
	}
	if err := seed.Close(); err != nil {
		t.Fatal(err)
	}
	// The "broken" current database the new version mangled.
	dbPath := filepath.Join(home, controlplane.DBFile)
	if err := os.WriteFile(dbPath, []byte("not sqlite at all"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(dbPath+"-wal", []byte("stale wal"), 0o600); err != nil {
		t.Fatal(err)
	}
	c, out, errOut := rollbackContext(home)
	if code := runDaemonRollback(ctx, c, nil); code != 0 {
		t.Fatalf("rollback = %d, stderr %s", code, errOut.String())
	}
	if !strings.Contains(out.String(), "database restored") {
		t.Errorf("stdout = %q", out.String())
	}
	restored, err := store.Open(ctx, dbPath, store.Options{})
	if err != nil {
		t.Fatalf("restored db does not open: %v", err)
	}
	if err := restored.Close(); err != nil {
		t.Error(err)
	}
	broken, err := filepath.Glob(dbPath + ".broken-*")
	if err != nil || len(broken) != 1 {
		t.Errorf("broken db set aside = %v, %v", broken, err)
	}
	if _, err := os.Stat(dbPath + "-wal"); !os.IsNotExist(err) {
		t.Errorf("stale wal survives: %v", err)
	}
}

func TestRollbackHelp(t *testing.T) {
	c, out, _ := rollbackContext(t.TempDir())
	if code := runDaemonRollback(context.Background(), c, []string{"--help"}); code != 0 {
		t.Fatalf("rollback --help = %d", code)
	}
	for _, want := range []string{"last-known-good", "NEVER", "forge restore"} {
		if !strings.Contains(out.String(), want) {
			t.Errorf("help lacks %q: %q", want, out.String())
		}
	}
}

func TestSameUTCDay(t *testing.T) {
	base := time.Date(2026, 8, 30, 23, 30, 0, 0, time.UTC)
	cases := []struct {
		name string
		a, b time.Time
		want bool
	}{
		{"same instant", base, base, true},
		{"same day", base, base.Add(-23 * time.Hour), true},
		{"across midnight", base, base.Add(time.Hour), false},
		{"zone folds to same UTC day", base, base.In(time.FixedZone("plus2", 7200)), true},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := sameUTCDay(c.a, c.b); got != c.want {
				t.Errorf("sameUTCDay(%v, %v) = %v", c.a, c.b, got)
			}
		})
	}
}
