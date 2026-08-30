package store

import (
	"context"
	"database/sql"
	"embed"
	"fmt"
	"io/fs"
	"sort"
	"strings"
)

//go:embed migrations/*.sql
var migrationFS embed.FS

// migration is one embedded SQL file. Files are named <ULID>_<slug>.sql so two
// branches never collide on a number and lexical order is creation order.
type migration struct {
	id  string
	sql string
}

func loadMigrations() ([]migration, error) {
	entries, err := fs.ReadDir(migrationFS, "migrations")
	if err != nil {
		return nil, fmt.Errorf("read embedded migrations: %w", err)
	}
	var out []migration
	for _, e := range entries {
		name := e.Name()
		if !strings.HasSuffix(name, ".sql") {
			continue
		}
		body, err := migrationFS.ReadFile("migrations/" + name)
		if err != nil {
			return nil, fmt.Errorf("read migration %s: %w", name, err)
		}
		out = append(out, migration{id: strings.TrimSuffix(name, ".sql"), sql: string(body)})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].id < out[j].id })
	return out, nil
}

// migrate applies every migration not yet recorded in schema_migrations, each in
// its own transaction, and records the last id as the schema version. A database
// that has a migration this binary does not know is refused: it belongs to a newer
// Forge, and writing to it could corrupt what that version expects.
func (s *Store) migrate(ctx context.Context) error {
	migrations, err := loadMigrations()
	if err != nil {
		return err
	}
	if len(migrations) == 0 {
		return fmt.Errorf("no embedded migrations")
	}
	if _, err := s.writer.ExecContext(ctx, `CREATE TABLE IF NOT EXISTS schema_migrations (id TEXT PRIMARY KEY, applied_at TEXT NOT NULL)`); err != nil {
		return fmt.Errorf("create schema_migrations: %w", err)
	}
	applied := map[string]bool{}
	err = each(s.writer.QueryContext(ctx, `SELECT id FROM schema_migrations`))(func(rows *sql.Rows) error {
		var id string
		if err := rows.Scan(&id); err != nil {
			return err
		}
		applied[id] = true
		return nil
	})
	if err != nil {
		return fmt.Errorf("read schema_migrations: %w", err)
	}
	known := map[string]bool{}
	for _, m := range migrations {
		known[m.id] = true
	}
	for id := range applied {
		if !known[id] {
			return fmt.Errorf("database has migration %s that this Forge does not know: it was written by a newer version", id)
		}
	}
	for _, m := range migrations {
		if applied[m.id] {
			continue
		}
		if err := s.applyMigration(ctx, m); err != nil {
			return err
		}
		s.log.InfoContext(ctx, "migration applied", "id", m.id)
	}
	s.schemaVersion = migrations[len(migrations)-1].id
	return nil
}

func (s *Store) applyMigration(ctx context.Context, m migration) (err error) {
	tx, err := s.writer.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("begin migration %s: %w", m.id, err)
	}
	defer func() {
		if err != nil {
			if rerr := tx.Rollback(); rerr != nil && rerr != sql.ErrTxDone {
				err = fmt.Errorf("%w (rollback: %v)", err, rerr)
			}
		}
	}()
	if _, err = tx.ExecContext(ctx, m.sql); err != nil {
		return fmt.Errorf("apply migration %s: %w", m.id, err)
	}
	if _, err = tx.ExecContext(ctx, `INSERT INTO schema_migrations (id, applied_at) VALUES (?, ?)`, m.id, formatTime(s.now())); err != nil {
		return fmt.Errorf("record migration %s: %w", m.id, err)
	}
	if err = tx.Commit(); err != nil {
		return fmt.Errorf("commit migration %s: %w", m.id, err)
	}
	return nil
}
