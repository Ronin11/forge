package store

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func openTest(t *testing.T) *Store {
	t.Helper()
	path := filepath.Join(t.TempDir(), "forge.sqlite3")
	fixed := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	s, err := Open(context.Background(), path, Options{Clock: func() time.Time { return fixed }})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := s.Close(); err != nil {
			t.Error(err)
		}
	})
	return s
}

func TestOpenMigratesAndIsIdempotent(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "sub", "forge.sqlite3")
	s, err := Open(ctx, path, Options{})
	if err != nil {
		t.Fatal(err)
	}
	if s.SchemaVersion() == "" {
		t.Error("no schema version")
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o600 {
		t.Errorf("database mode = %v", info.Mode().Perm())
	}
	var mode string
	if err := s.queryRow(ctx, `PRAGMA journal_mode`).Scan(&mode); err != nil || mode != "wal" {
		t.Errorf("journal_mode = %q, %v", mode, err)
	}
	var fk int
	if err := s.queryRow(ctx, `PRAGMA foreign_keys`).Scan(&fk); err != nil || fk != 1 {
		t.Errorf("foreign_keys = %d, %v", fk, err)
	}
	var n int
	if err := s.queryRow(ctx, `SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ('journal','attempt_facts','plugins','kb_fts','path_leases')`).Scan(&n); err != nil || n != 5 {
		t.Errorf("tables present = %d, %v", n, err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	again, err := Open(ctx, path, Options{})
	if err != nil {
		t.Fatalf("reopen: %v", err)
	}
	var applied int
	if err := again.queryRow(ctx, `SELECT count(*) FROM schema_migrations`).Scan(&applied); err != nil || applied != 1 {
		t.Errorf("applied migrations = %d, %v", applied, err)
	}
	if err := again.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestOpenRefusesNewerSchema(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "forge.sqlite3")
	s, err := Open(ctx, path, Options{})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := s.writer.ExecContext(ctx, `INSERT INTO schema_migrations (id, applied_at) VALUES ('ZZZZZZZZZZZZZZZZZZZZZZZZZZ_future', '2099-01-01T00:00:00Z')`); err != nil {
		t.Fatal(err)
	}
	if err := s.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := Open(ctx, path, Options{}); err == nil {
		t.Fatal("opened a database from a newer Forge")
	}
}

func TestWriteJournalAndRollback(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	err := s.Write(ctx, func(tx *Tx) error {
		return tx.Journal(ctx, "daemon.started", EntityDaemon, "daemon", map[string]any{"pid": 42})
	})
	if err != nil {
		t.Fatal(err)
	}
	boom := errors.New("boom")
	err = s.Write(ctx, func(tx *Tx) error {
		if err := tx.Journal(ctx, "daemon.draining", EntityDaemon, "daemon", nil); err != nil {
			return err
		}
		return boom
	})
	if !errors.Is(err, boom) {
		t.Fatalf("Write returned %v", err)
	}
	entries, err := s.JournalSince(ctx, 0, 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 1 || entries[0].ID != 1 || entries[0].Kind != "daemon.started" || !entries[0].Time.Equal(time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)) {
		t.Fatalf("journal = %+v", entries)
	}
	var payload map[string]any
	if err := json.Unmarshal(entries[0].Payload, &payload); err != nil || payload["pid"] != float64(42) {
		t.Errorf("payload = %s, %v", entries[0].Payload, err)
	}
	if more, err := s.JournalSince(ctx, 1, 10); err != nil || len(more) != 0 {
		t.Errorf("cursor: %v %v", more, err)
	}
	hist, err := s.JournalForEntity(ctx, EntityDaemon, "daemon")
	if err != nil || len(hist) != 1 {
		t.Errorf("history: %v %v", hist, err)
	}
}

func TestWritesAreSerialisedAndReadable(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	done := make(chan error, 8)
	for i := 0; i < 8; i++ {
		go func(i int) {
			done <- s.Write(ctx, func(tx *Tx) error {
				return tx.Journal(ctx, "test", EntityDaemon, "x", i)
			})
		}(i)
	}
	for i := 0; i < 8; i++ {
		if err := <-done; err != nil {
			t.Fatal(err)
		}
	}
	entries, err := s.JournalSince(ctx, 0, 100)
	if err != nil || len(entries) != 8 {
		t.Fatalf("got %d entries, %v", len(entries), err)
	}
	for i, e := range entries {
		if e.ID != int64(i+1) {
			t.Errorf("ids not monotonic: %v", e.ID)
		}
	}
}
