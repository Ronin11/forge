// Package store is the only place SQL lives. One process — the daemon — opens the
// database; everything else reaches it through the API. Writes go through a single
// connection inside explicit transactions (Write); reads use a small pool. Every
// state change of a Work, Target, Attempt, Question, or Proposal also writes a
// journal row in the same transaction (Tx.Journal).
package store

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"log/slog"
	"net/url"
	"os"
	"path/filepath"
	"sync"
	"time"

	_ "modernc.org/sqlite" // the driver; pure Go so the binary needs no cgo
)

// Store owns the database file.
type Store struct {
	path string
	log  *slog.Logger
	now  func() time.Time

	// writeMu serialises Write so the single writer connection is never shared by
	// two transactions; SQLite would serialise them anyway, this makes the order
	// explicit and the busy_timeout rarely relevant.
	writeMu sync.Mutex
	writer  *sql.DB // MaxOpenConns(1)
	reader  *sql.DB

	schemaVersion string
}

// Options configure Open. Clock defaults to time.Now.
type Options struct {
	Logger *slog.Logger
	Clock  func() time.Time
}

// Open creates or opens the database at path (0600, parent created 0700), applies
// pragmas, and runs pending migrations.
func Open(ctx context.Context, path string, opts Options) (*Store, error) {
	if opts.Logger == nil {
		opts.Logger = slog.New(slog.DiscardHandler)
	}
	if opts.Clock == nil {
		opts.Clock = time.Now
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, fmt.Errorf("create database directory: %w", err)
	}
	// Create the file ourselves so its mode is 0600 from the first byte.
	f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, fmt.Errorf("create database %s: %w", path, err)
	}
	if err := f.Close(); err != nil {
		return nil, fmt.Errorf("close database %s: %w", path, err)
	}
	dsn := "file:" + path + "?" + url.Values{
		"_pragma": {"busy_timeout(5000)", "journal_mode(WAL)", "synchronous(NORMAL)", "foreign_keys(1)"},
	}.Encode()
	writer, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, fmt.Errorf("open database writer: %w", err)
	}
	writer.SetMaxOpenConns(1)
	writer.SetMaxIdleConns(1)
	writer.SetConnMaxLifetime(0)
	reader, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, fmt.Errorf("open database reader: %w", err)
	}
	reader.SetMaxOpenConns(4)
	s := &Store{path: path, log: opts.Logger, now: opts.Clock, writer: writer, reader: reader}
	if err := s.migrate(ctx); err != nil {
		if cerr := s.Close(); cerr != nil {
			s.log.WarnContext(ctx, "close after failed migration", "error", cerr)
		}
		return nil, err
	}
	if err := s.backfillWorkflowGraphs(ctx); err != nil {
		if cerr := s.Close(); cerr != nil {
			s.log.WarnContext(ctx, "close after failed backfill", "error", cerr)
		}
		return nil, fmt.Errorf("backfill workflow graphs: %w", err)
	}
	return s, nil
}

// Close releases both pools.
func (s *Store) Close() error {
	werr := s.writer.Close()
	rerr := s.reader.Close()
	if werr != nil {
		return fmt.Errorf("close writer: %w", werr)
	}
	if rerr != nil {
		return fmt.Errorf("close reader: %w", rerr)
	}
	return nil
}

// Path is the database file.
func (s *Store) Path() string { return s.path }

// SchemaVersion is the id of the last applied migration; the handshake reports it.
func (s *Store) SchemaVersion() string { return s.schemaVersion }

// Tx is one write transaction. Every store method that changes state takes a Tx
// so callers compose changes and journal rows atomically.
type Tx struct {
	tx  *sql.Tx
	now time.Time
	log *slog.Logger
}

// Write runs fn in one BEGIN IMMEDIATE transaction on the writer connection and
// commits if fn returns nil. It is the only way to change the database.
func (s *Store) Write(ctx context.Context, fn func(tx *Tx) error) (err error) {
	s.writeMu.Lock()
	defer s.writeMu.Unlock()
	start := time.Now()
	sqlTx, err := s.writer.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("begin write: %w", err)
	}
	tx := &Tx{tx: sqlTx, now: s.now().UTC(), log: s.log}
	defer func() {
		if err != nil {
			if rerr := sqlTx.Rollback(); rerr != nil && rerr != sql.ErrTxDone {
				err = fmt.Errorf("%w (rollback: %v)", err, rerr)
			}
		}
	}()
	if err = fn(tx); err != nil {
		return err
	}
	if err = sqlTx.Commit(); err != nil {
		return fmt.Errorf("commit: %w", err)
	}
	s.log.Log(ctx, levelTrace, "write committed", "duration_us", time.Since(start).Microseconds())
	return nil
}

// levelTrace mirrors logging.LevelTrace without importing it (store stays low in
// the layering; the value is the contract).
const levelTrace = slog.Level(-8)

// Now is the transaction's timestamp; every row written in one Tx shares it.
func (tx *Tx) Now() time.Time { return tx.now }

// Exec runs a statement inside the transaction, tracing the SQL.
func (tx *Tx) Exec(ctx context.Context, query string, args ...any) (sql.Result, error) {
	tx.log.Log(ctx, levelTrace, "sql", "stmt", query)
	return tx.tx.ExecContext(ctx, query, args...)
}

// Query runs a read inside the transaction (for read-modify-write).
func (tx *Tx) Query(ctx context.Context, query string, args ...any) (*sql.Rows, error) {
	tx.log.Log(ctx, levelTrace, "sql", "stmt", query)
	return tx.tx.QueryContext(ctx, query, args...)
}

// QueryRow runs a single-row read inside the transaction.
func (tx *Tx) QueryRow(ctx context.Context, query string, args ...any) *sql.Row {
	tx.log.Log(ctx, levelTrace, "sql", "stmt", query)
	return tx.tx.QueryRowContext(ctx, query, args...)
}

// Prepare prepares a statement for repeated execution inside this transaction
// (event batches).
func (tx *Tx) Prepare(ctx context.Context, query string) (*sql.Stmt, error) {
	tx.log.Log(ctx, levelTrace, "sql prepare", "stmt", query)
	return tx.tx.PrepareContext(ctx, query)
}

// query runs a read on the pool.
func (s *Store) query(ctx context.Context, query string, args ...any) (*sql.Rows, error) {
	s.log.Log(ctx, levelTrace, "sql", "stmt", query)
	return s.reader.QueryContext(ctx, query, args...)
}

// queryRow runs a single-row read on the pool.
func (s *Store) queryRow(ctx context.Context, query string, args ...any) *sql.Row {
	s.log.Log(ctx, levelTrace, "sql", "stmt", query)
	return s.reader.QueryRowContext(ctx, query, args...)
}

// formatTime is the one timestamp format in the database.
func formatTime(t time.Time) string { return t.UTC().Format(time.RFC3339Nano) }

// parseTime reads what formatTime wrote; NULL yields the zero time.
func parseTime(s sql.NullString) (time.Time, error) {
	if !s.Valid || s.String == "" {
		return time.Time{}, nil
	}
	t, err := time.Parse(time.RFC3339Nano, s.String)
	if err != nil {
		return time.Time{}, fmt.Errorf("parse timestamp %q: %w", s.String, err)
	}
	return t, nil
}

// nullTime is the write-side counterpart of parseTime.
func nullTime(t time.Time) any {
	if t.IsZero() {
		return nil
	}
	return formatTime(t)
}

// nullString stores "" as NULL so optional columns stay optional.
func nullString(s string) any {
	if s == "" {
		return nil
	}
	return s
}

// boolInt is how booleans are stored.
func boolInt(b bool) int {
	if b {
		return 1
	}
	return 0
}

// each iterates rows, calling scan per row, and always closes them, joining the
// close error with any scan error so nothing is discarded. Callers pass the
// result of query directly: each(s.query(ctx, q, args...))(scan).
func each(rows *sql.Rows, err error) func(scan func(*sql.Rows) error) error {
	return func(scan func(*sql.Rows) error) error {
		if err != nil {
			return err
		}
		for rows.Next() {
			if err := scan(rows); err != nil {
				return errors.Join(err, rows.Close())
			}
		}
		return errors.Join(rows.Err(), rows.Close())
	}
}
