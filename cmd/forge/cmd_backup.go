// forge backup asks the running daemon (the sole opener of the database) to
// write a single-file archive; forge restore unpacks one into a fresh
// FORGE_HOME (DESIGN.md §23).
package main

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"path/filepath"

	"forge/internal/controlplane"
)

func runBackup(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("backup")
	out := fs.String("out", "", "directory the archive lands in (default <home>/backups)")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() > 0 {
		fmt.Fprintf(c.stderr, "forge backup: unexpected argument %q\n", fs.Arg(0))
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.backup")
	if code >= 0 {
		return code
	}
	dir := *out
	if dir != "" {
		abs, err := filepath.Abs(dir)
		if err != nil {
			return c.fail("backup", err)
		}
		dir = abs
	}
	cl := c.client(log)
	cl.noAutoStart = true
	if err := cl.connect(ctx); err != nil {
		fmt.Fprintln(c.stderr, "forge backup: the daemon is not running; it owns the database, so backup needs it (forge daemon start)")
		return 1
	}
	var resp struct {
		Archive string `json:"archive"`
		Bytes   int64  `json:"bytes"`
	}
	if err := cl.do(ctx, http.MethodPost, "/api/v1/backup", map[string]string{"out": dir}, &resp); err != nil {
		return c.fail("backup", err)
	}
	if *asJSON {
		c.printJSON(resp)
		return 0
	}
	fmt.Fprintf(c.stdout, "backup written: %s (%d bytes)\n", resp.Archive, resp.Bytes)
	return 0
}

func runRestore(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("restore")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.stderr, "usage: forge restore ARCHIVE  (with FORGE_HOME pointing at a fresh, empty home)")
		return 2
	}
	if _, _, code := c.resolveLogging(lf, "cli.restore"); code >= 0 {
		return code
	}
	archive, err := filepath.Abs(fs.Arg(0))
	if err != nil {
		return c.fail("restore", err)
	}
	home := c.forgeHome
	// Fresh-home restores only: an empty (or absent) home cannot hold a
	// daemon.lock, so no running daemon can be pulled out from under.
	entries, err := os.ReadDir(home)
	switch {
	case os.IsNotExist(err):
		if err := os.MkdirAll(home, 0o700); err != nil {
			return c.fail("restore", err)
		}
	case err != nil:
		return c.fail("restore", err)
	case len(entries) > 0:
		fmt.Fprintf(c.stderr, "forge restore: %s is not empty; restore only into a fresh FORGE_HOME (point FORGE_HOME at a new directory, or move the old home aside)\n", home)
		return 1
	}
	if err := controlplane.UnpackBackup(archive, home); err != nil {
		return c.fail("restore", err)
	}
	fmt.Fprintf(c.stdout, "restored %s into %s\n", filepath.Base(archive), home)
	fmt.Fprintln(c.stdout, "next steps:")
	fmt.Fprintln(c.stdout, "  1. review config.toml and worker.toml — their paths name the home they were backed up from")
	fmt.Fprintln(c.stdout, "  2. forge daemon start")
	return 0
}
