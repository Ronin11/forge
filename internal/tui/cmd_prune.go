package tui

import (
	"context"
	"fmt"
	"path/filepath"

	"forge/internal/core/config"
	"forge/internal/core/engine"
	"forge/internal/core/worker"
)

// RunPrune applies the retention policy to raw output and artifacts (rows are
// forever, DESIGN.md §9.3). Files live under the worker's data dir; the CLI
// reads worker.toml like `forge cleanup` does.
func RunPrune(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("prune")
	dryRun := fs.Bool("dry-run", false, "report what would be deleted")
	confirm := fs.Bool("confirm", false, "delete")
	cfgPath := fs.String("config", filepath.Join(c.ForgeHome, "worker.toml"), "worker configuration")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if *dryRun == *confirm {
		fmt.Fprintln(c.Stderr, "forge prune: exactly one of --dry-run or --confirm is required")
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.prune")
	if code >= 0 {
		return code
	}
	wcfg, err := worker.LoadConfig(*cfgPath)
	if err != nil {
		return c.Fail("prune", err)
	}
	dcfg, err := config.LoadConfig(filepath.Join(c.ForgeHome, "config.toml"), c.ForgeHome, c.UserHome, c.Getenv)
	if err != nil {
		return c.Fail("prune", err)
	}
	rep, err := engine.Prune(ctx, engine.PruneInput{
		DataDir: wcfg.DataDir, Retention: dcfg.Retention, Now: c.Now(), Delete: *confirm, Logger: log,
	})
	if err != nil {
		return c.Fail("prune", err)
	}
	verb := "would delete"
	if *confirm {
		verb = "deleted"
	}
	fmt.Fprintf(c.Stdout, "%s %d output file(s) (%d bytes), %d artifact dir(s); compressed %d transcript(s)\n", verb, rep.Outputs, rep.OutputBytes, rep.ArtifactDirs, rep.Compressed)
	return 0
}
