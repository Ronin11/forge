// forge backup asks the running daemon (the sole opener of the database) to
// write a single-file archive; forge restore unpacks one into a fresh
// FORGE_HOME (DESIGN.md §23).
package tui

import (
	"context"
	"fmt"
	"forge/internal/core/engine"
	"net/http"
	"os"
	"path/filepath"
)

func RunBackup(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("backup")
	out := fs.String("out", "", "directory the archive lands in (default <home>/backups)")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() > 0 {
		fmt.Fprintf(c.Stderr, "forge backup: unexpected argument %q\n", fs.Arg(0))
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.backup")
	if code >= 0 {
		return code
	}
	dir := *out
	if dir != "" {
		abs, err := filepath.Abs(dir)
		if err != nil {
			return c.Fail("backup", err)
		}
		dir = abs
	}
	cl := c.Client(log)
	cl.NoAutoStart = true
	if err := cl.Connect(ctx); err != nil {
		fmt.Fprintln(c.Stderr, "forge backup: the daemon is not running; it owns the database, so backup needs it (forge daemon start)")
		return 1
	}
	var resp struct {
		Archive string `json:"archive"`
		Bytes   int64  `json:"bytes"`
	}
	if err := cl.Do(ctx, http.MethodPost, "/api/v1/backup", map[string]string{"out": dir}, &resp); err != nil {
		return c.Fail("backup", err)
	}
	if *asJSON {
		c.PrintJSON(resp)
		return 0
	}
	fmt.Fprintf(c.Stdout, "backup written: %s (%d bytes)\n", resp.Archive, resp.Bytes)
	return 0
}

func RunRestore(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("restore")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.Stderr, "usage: forge restore ARCHIVE  (with FORGE_HOME pointing at a fresh, empty home)")
		return 2
	}
	if _, _, code := c.ResolveLogging(lf, "cli.restore"); code >= 0 {
		return code
	}
	archive, err := filepath.Abs(fs.Arg(0))
	if err != nil {
		return c.Fail("restore", err)
	}
	home := c.ForgeHome
	// Fresh-home restores only: an empty (or absent) home cannot hold a
	// daemon.lock, so no running daemon can be pulled out from under.
	entries, err := os.ReadDir(home)
	switch {
	case os.IsNotExist(err):
		if err := os.MkdirAll(home, 0o700); err != nil {
			return c.Fail("restore", err)
		}
	case err != nil:
		return c.Fail("restore", err)
	case len(entries) > 0:
		fmt.Fprintf(c.Stderr, "forge restore: %s is not empty; restore only into a fresh FORGE_HOME (point FORGE_HOME at a new directory, or move the old home aside)\n", home)
		return 1
	}
	if err := engine.UnpackBackup(archive, home); err != nil {
		return c.Fail("restore", err)
	}
	fmt.Fprintf(c.Stdout, "restored %s into %s\n", filepath.Base(archive), home)
	fmt.Fprintln(c.Stdout, "next steps:")
	fmt.Fprintln(c.Stdout, "  1. review config.toml and worker.toml — their paths name the home they were backed up from")
	fmt.Fprintln(c.Stdout, "  2. forge daemon start")
	return 0
}
