package store

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
)

// BackupInto writes a consistent snapshot of the database to dbPath using
// SQLite's VACUUM INTO, which copies committed state (including anything in
// the WAL) without blocking writers for the duration. It runs on the read
// pool so a long backup never queues behind the writer. The destination must
// not exist — VACUUM INTO refuses to overwrite, and so does this method.
func (s *Store) BackupInto(ctx context.Context, dbPath string) error {
	if dbPath == "" {
		return fmt.Errorf("backup: destination path is required")
	}
	if _, err := os.Stat(dbPath); err == nil {
		return fmt.Errorf("backup: %s already exists", dbPath)
	} else if !os.IsNotExist(err) {
		return fmt.Errorf("backup: stat %s: %w", dbPath, err)
	}
	if err := os.MkdirAll(filepath.Dir(dbPath), 0o700); err != nil {
		return fmt.Errorf("backup: create directory for %s: %w", dbPath, err)
	}
	if _, err := s.reader.ExecContext(ctx, `VACUUM INTO ?`, dbPath); err != nil {
		return fmt.Errorf("backup into %s: %w", dbPath, err)
	}
	if err := os.Chmod(dbPath, 0o600); err != nil {
		return fmt.Errorf("backup: chmod %s: %w", dbPath, err)
	}
	return nil
}

// preMigrationSnapshot copies the database aside before a pending migration
// touches an existing database, so a migration that goes wrong leaves
// <db>.pre-<migration id> to fall back to. VACUUM INTO (not a file copy)
// because a WAL-mode database is two files; the snapshot is one consistent
// one. A stale snapshot with the same name is replaced — the newest attempt
// at this migration is the one worth keeping.
func (s *Store) preMigrationSnapshot(ctx context.Context, migrationID string) error {
	path := s.path + ".pre-" + migrationID
	if err := os.Remove(path); err != nil && !os.IsNotExist(err) {
		return fmt.Errorf("migration backup: remove stale %s: %w", path, err)
	}
	if _, err := s.writer.ExecContext(ctx, `VACUUM INTO ?`, path); err != nil {
		return fmt.Errorf("migration backup into %s: %w", path, err)
	}
	s.log.InfoContext(ctx, "database backed up before migration", "id", migrationID, "path", path)
	return nil
}
