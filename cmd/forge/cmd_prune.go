package main

import (
	"context"
	"fmt"
	"path/filepath"

	"forge/internal/controlplane"
	"forge/internal/core/config"
	"forge/internal/core/worker"
)

// runPrune applies the retention policy to raw output and artifacts (rows are
// forever, DESIGN.md §9.3). Files live under the worker's data dir; the CLI
// reads worker.toml like `forge cleanup` does.
func runPrune(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("prune")
	dryRun := fs.Bool("dry-run", false, "report what would be deleted")
	confirm := fs.Bool("confirm", false, "delete")
	cfgPath := fs.String("config", filepath.Join(c.forgeHome, "worker.toml"), "worker configuration")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if *dryRun == *confirm {
		fmt.Fprintln(c.stderr, "forge prune: exactly one of --dry-run or --confirm is required")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.prune")
	if code >= 0 {
		return code
	}
	wcfg, err := worker.LoadConfig(*cfgPath)
	if err != nil {
		return c.fail("prune", err)
	}
	dcfg, err := config.LoadConfig(filepath.Join(c.forgeHome, "config.toml"), c.forgeHome, c.userHome, c.getenv)
	if err != nil {
		return c.fail("prune", err)
	}
	rep, err := controlplane.Prune(ctx, controlplane.PruneInput{
		DataDir: wcfg.DataDir, Retention: dcfg.Retention, Now: c.now(), Delete: *confirm, Logger: log,
	})
	if err != nil {
		return c.fail("prune", err)
	}
	verb := "would delete"
	if *confirm {
		verb = "deleted"
	}
	fmt.Fprintf(c.stdout, "%s %d output file(s) (%d bytes), %d artifact dir(s); compressed %d transcript(s)\n", verb, rep.Outputs, rep.OutputBytes, rep.ArtifactDirs, rep.Compressed)
	return 0
}
