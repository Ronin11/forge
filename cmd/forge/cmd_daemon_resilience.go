// Daemon-side resilience (DESIGN.md §23): the post-start self-check, the
// last-known-good snapshot every healthy start refreshes, the nightly backup
// loop, and `forge daemon rollback`.
package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"time"

	"forge/internal/controlplane"
	"forge/internal/store"
)

// Names under <home>/prev: what a rollback restores.
const (
	prevDirName = "prev"
	prevBinName = "forge-bin-good"
	prevDBName  = "db-good"
)

// selfCheck is the post-start gate: bootstrap ran and the listeners are bound
// by the time this is called, so what remains is proving the store answers a
// real read. A version whose migrations or schema are broken fails here,
// before the daemon claims to be healthy.
func (d *daemonProcess) selfCheck(ctx context.Context, st *store.Store) error {
	if _, err := st.Projects(ctx); err != nil {
		return fmt.Errorf("post-start self-check: store read: %w", err)
	}
	return nil
}

// captureLastKnownGood refreshes <home>/prev after a healthy start: a copy of
// the running binary and a VACUUM INTO snapshot of the database. `forge
// daemon rollback` restores exactly these. Called only after selfCheck
// passed, so "good" means "served at least once".
func captureLastKnownGood(ctx context.Context, st *store.Store, home, exe string) error {
	prev := filepath.Join(home, prevDirName)
	if err := os.MkdirAll(prev, 0o700); err != nil {
		return fmt.Errorf("create %s: %w", prev, err)
	}
	if err := copyBinary(exe, filepath.Join(prev, prevBinName)); err != nil {
		return err
	}
	db := filepath.Join(prev, prevDBName)
	if err := os.Remove(db); err != nil && !os.IsNotExist(err) {
		return fmt.Errorf("replace %s: %w", db, err)
	}
	return st.BackupInto(ctx, db)
}

// copyBinary copies src over dst via a temp file + rename so a crash mid-copy
// never leaves a truncated "good" binary.
func copyBinary(src, dst string) (err error) {
	in, err := os.Open(src)
	if err != nil {
		return fmt.Errorf("open %s: %w", src, err)
	}
	defer func() { err = errors.Join(err, in.Close()) }()
	tmp := dst + ".tmp"
	out, err := os.OpenFile(tmp, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o700)
	if err != nil {
		return fmt.Errorf("create %s: %w", tmp, err)
	}
	if _, cerr := io.Copy(out, in); cerr != nil {
		return errors.Join(fmt.Errorf("copy %s: %w", src, cerr), out.Close())
	}
	if err := out.Close(); err != nil {
		return err
	}
	if err := os.Rename(tmp, dst); err != nil {
		return fmt.Errorf("rename %s: %w", tmp, err)
	}
	return nil
}

// nightlyBackup writes an archive to <home>/backups once a day (immediately
// on start when today has none yet) and prunes past [backup] keep.
func (d *daemonProcess) nightlyBackup(ctx context.Context, st *store.Store) {
	log := d.handler.For("daemon.backup")
	dir := filepath.Join(d.c.forgeHome, "backups")
	write := func() {
		archive, err := controlplane.WriteBackupArchive(ctx, st, controlplane.BackupInputs{
			Home: d.c.forgeHome, KbDir: d.cfg.KB.Path, OutDir: dir,
		})
		if err != nil {
			log.WarnContext(ctx, "nightly backup", "error", err)
			return
		}
		if err := st.Write(ctx, func(tx *store.Tx) error {
			return tx.Journal(ctx, "daemon.backup", store.EntityDaemon, "daemon", map[string]any{"archive": archive, "nightly": true})
		}); err != nil {
			log.WarnContext(ctx, "journal backup", "error", err)
		}
		removed, err := controlplane.PruneBackups(dir, d.cfg.Backup.Keep)
		if err != nil {
			log.WarnContext(ctx, "prune backups", "error", err)
		}
		log.InfoContext(ctx, "nightly backup written", "archive", filepath.Base(archive), "pruned", len(removed))
	}
	if _, mtime, err := controlplane.LatestBackup(dir); err != nil {
		log.WarnContext(ctx, "read backups", "error", err)
	} else if mtime.IsZero() || !sameUTCDay(mtime, time.Now()) {
		write()
	}
	t := time.NewTicker(24 * time.Hour)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			write()
		}
	}
}

// sameUTCDay reports whether two instants fall on the same UTC calendar day —
// the "one backup a day" rule compares external file mtimes, so it is
// necessarily wall-clock.
func sameUTCDay(a, b time.Time) bool {
	ay, am, ad := a.UTC().Date()
	by, bm, bd := b.UTC().Date()
	return ay == by && am == bm && ad == bd
}

const rollbackHelp = `usage: forge daemon rollback

Restores the last-known-good database snapshot the previous healthy daemon
start recorded under <home>/prev, after backing the current database aside as
forge.sqlite3.broken-<ts>. The daemon must be stopped.

Limits (deliberate):
  - only the database and a pointer to the previous binary are restored;
    everything written since the last healthy start is lost from the db
  - the previous binary is saved at <home>/prev/forge-bin-good but is NEVER
    copied over your forge binary — run it directly or copy it yourself
  - kb/, modes/, and config files are untouched; use forge restore for those
`

func runDaemonRollback(ctx context.Context, c *cmdContext, args []string) int {
	for _, a := range args {
		if a == "-h" || a == "--help" {
			fmt.Fprint(c.stdout, rollbackHelp)
			return 0
		}
	}
	fs, lf := c.flags("daemon rollback")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() > 0 {
		fmt.Fprintf(c.stderr, "forge daemon rollback: unexpected argument %q\n", fs.Arg(0))
		return 2
	}
	if _, _, code := c.resolveLogging(lf, "cli.daemon"); code >= 0 {
		return code
	}
	home := c.forgeHome
	locked, err := controlplane.IsLocked(home)
	if err != nil {
		return c.fail("daemon rollback", err)
	}
	if locked {
		fmt.Fprintln(c.stderr, "forge daemon rollback: the daemon is running; stop it first (forge daemon stop)")
		return 1
	}
	goodDB := filepath.Join(home, prevDirName, prevDBName)
	if _, err := os.Stat(goodDB); err != nil {
		fmt.Fprintf(c.stderr, "forge daemon rollback: no last-known-good snapshot at %s (a healthy daemon start records one)\n", goodDB)
		return 1
	}
	dbPath := filepath.Join(home, controlplane.DBFile)
	ts := c.now().UTC().Format("20060102T150405Z")
	if _, err := os.Stat(dbPath); err == nil {
		broken := dbPath + ".broken-" + ts
		if err := os.Rename(dbPath, broken); err != nil {
			return c.fail("daemon rollback", err)
		}
		fmt.Fprintf(c.stdout, "current database moved aside: %s\n", broken)
	}
	for _, suffix := range []string{"-wal", "-shm"} {
		if err := os.Remove(dbPath + suffix); err != nil && !os.IsNotExist(err) {
			return c.fail("daemon rollback", err)
		}
	}
	if err := copyBinary(goodDB, dbPath); err != nil {
		return c.fail("daemon rollback", err)
	}
	if err := os.Chmod(dbPath, 0o600); err != nil {
		return c.fail("daemon rollback", err)
	}
	goodBin := filepath.Join(home, prevDirName, prevBinName)
	fmt.Fprintf(c.stdout, "database restored from %s\n", goodDB)
	if _, err := os.Stat(goodBin); err == nil {
		fmt.Fprintf(c.stdout, "previous binary: %s — run it directly or copy it over your forge binary yourself; rollback never overwrites binaries\n", goodBin)
	}
	fmt.Fprintln(c.stdout, "next: forge daemon start (with the binary you trust)")
	return 0
}
