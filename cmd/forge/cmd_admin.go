package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"

	"forge/internal/core/config"
	"forge/internal/core/daemon"
	"forge/internal/core/migratedirectives"
	"forge/internal/core/store"
	"forge/internal/tui"
)

// runAdmin is maintenance surgery that opens the database directly — the
// rehearsal path for migrations. It refuses to run under a live daemon.
func runAdmin(ctx context.Context, c *tui.Context, args []string) int {
	if len(args) == 0 || args[0] != "migrate-directives" {
		fmt.Fprintln(c.Stderr, "usage: forge admin migrate-directives [--home DIR] [--dry-run]")
		return 2
	}
	fs, lf := c.Flags("admin migrate-directives")
	home := fs.String("home", c.ForgeHome, "forge home to migrate (rehearse against a copy)")
	dryRun := fs.Bool("dry-run", false, "report what would change without writing")
	if code := c.Parse(fs, args[1:]); code >= 0 {
		return code
	}
	_, log, code := c.ResolveLogging(lf, "cli.admin")
	if code >= 0 {
		return code
	}
	// One writer: a daemon on this home owns the database.
	lock, err := daemon.TryLock(*home)
	if errors.Is(err, daemon.ErrLocked) {
		return c.Fail("admin", fmt.Errorf("a daemon owns %s — stop it (or point --home at a copy)", *home))
	}
	if err != nil {
		return c.Fail("admin", err)
	}
	defer func() {
		if err := lock.Release(); err != nil {
			fmt.Fprintln(c.Stderr, "forge admin: release lock:", err)
		}
	}()
	cfg, err := config.LoadConfig(filepath.Join(*home, "config.toml"), *home, c.UserHome, c.Getenv)
	if err != nil {
		return c.Fail("admin", err)
	}
	st, err := store.Open(ctx, filepath.Join(*home, daemon.DBFile), store.Options{})
	if err != nil {
		return c.Fail("admin", err)
	}
	defer func() {
		if err := st.Close(); err != nil {
			fmt.Fprintln(c.Stderr, "forge admin: close store:", err)
		}
	}()
	rep, err := migratedirectives.Run(ctx, st, *home, cfg.Prompts.Path, *dryRun, log)
	if err != nil {
		return c.Fail("admin migrate-directives", err)
	}
	out, err := json.MarshalIndent(rep, "", "  ")
	if err != nil {
		return c.Fail("admin", err)
	}
	verb := "migrated"
	if *dryRun {
		verb = "would migrate"
	}
	fmt.Fprintf(c.Stdout, "%s (%s):\n%s\n", verb, cfg.Prompts.Path, out)
	if rep.Empty() {
		fmt.Fprintln(c.Stdout, "nothing to do — already migrated (or a fresh home)")
	}
	return 0
}
