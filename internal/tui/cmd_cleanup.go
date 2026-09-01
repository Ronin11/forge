package tui

import (
	"context"
	"fmt"
	"path/filepath"
	"strings"

	"forge/internal/core/worker"
)

// RunCleanup is the worker-local `forge cleanup ATTEMPT [--confirm]`: preview a
// retained worktree, then remove it with --force. The branch is always kept.
func RunCleanup(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("cleanup")
	confirm := fs.Bool("confirm", false, "remove the worktree (uncommitted changes shown in the preview are lost)")
	cfgPath := fs.String("config", filepath.Join(c.ForgeHome, "worker.toml"), "worker configuration")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.Stderr, "usage: forge cleanup ATTEMPT_ID [--confirm]")
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.cleanup")
	if code >= 0 {
		return code
	}
	cfg, err := worker.LoadConfig(*cfgPath)
	if err != nil {
		return c.Fail("cleanup", err)
	}
	id, err := worker.ResolveAttemptID(cfg.DataDir, fs.Arg(0))
	if err != nil {
		return c.Fail("cleanup", err)
	}
	preview, err := worker.CleanupPreview(ctx, cfg, id)
	if err != nil {
		return c.Fail("cleanup", err)
	}
	fmt.Fprintf(c.Stdout, "attempt %s\n  worktree %s\n  branch %s (kept)\n  lifecycle %s\n  reason %s\n  path exists %v, registered %v, dirty %v\n", id, preview.Path, preview.Branch, preview.Lifecycle, preview.Reason, preview.PathExists, preview.Registered, preview.Dirty)
	if strings.TrimSpace(preview.Status) != "" {
		fmt.Fprintf(c.Stdout, "  status:\n    %s\n", strings.ReplaceAll(strings.TrimSpace(preview.Status), "\n", "\n    "))
	}
	if !*confirm {
		fmt.Fprintf(c.Stdout, "preview only; add --confirm to remove the worktree\n")
		return 0
	}
	if err := worker.CleanupConfirm(ctx, cfg, id, log); err != nil {
		return c.Fail("cleanup", err)
	}
	fmt.Fprintf(c.Stdout, "worktree removed; branch %s kept\n", preview.Branch)
	return 0
}
