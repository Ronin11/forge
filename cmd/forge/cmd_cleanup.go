package main

import (
	"context"
	"fmt"
	"path/filepath"
	"strings"

	"forge/internal/core/worker"
)

// runCleanup is the worker-local `forge cleanup ATTEMPT [--confirm]`: preview a
// retained worktree, then remove it with --force. The branch is always kept.
func runCleanup(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("cleanup")
	confirm := fs.Bool("confirm", false, "remove the worktree (uncommitted changes shown in the preview are lost)")
	cfgPath := fs.String("config", filepath.Join(c.forgeHome, "worker.toml"), "worker configuration")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.stderr, "usage: forge cleanup ATTEMPT_ID [--confirm]")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.cleanup")
	if code >= 0 {
		return code
	}
	cfg, err := worker.LoadConfig(*cfgPath)
	if err != nil {
		return c.fail("cleanup", err)
	}
	id, err := worker.ResolveAttemptID(cfg.DataDir, fs.Arg(0))
	if err != nil {
		return c.fail("cleanup", err)
	}
	preview, err := worker.CleanupPreview(ctx, cfg, id)
	if err != nil {
		return c.fail("cleanup", err)
	}
	fmt.Fprintf(c.stdout, "attempt %s\n  worktree %s\n  branch %s (kept)\n  lifecycle %s\n  reason %s\n  path exists %v, registered %v, dirty %v\n", id, preview.Path, preview.Branch, preview.Lifecycle, preview.Reason, preview.PathExists, preview.Registered, preview.Dirty)
	if strings.TrimSpace(preview.Status) != "" {
		fmt.Fprintf(c.stdout, "  status:\n    %s\n", strings.ReplaceAll(strings.TrimSpace(preview.Status), "\n", "\n    "))
	}
	if !*confirm {
		fmt.Fprintf(c.stdout, "preview only; add --confirm to remove the worktree\n")
		return 0
	}
	if err := worker.CleanupConfirm(ctx, cfg, id, log); err != nil {
		return c.fail("cleanup", err)
	}
	fmt.Fprintf(c.stdout, "worktree removed; branch %s kept\n", preview.Branch)
	return 0
}
