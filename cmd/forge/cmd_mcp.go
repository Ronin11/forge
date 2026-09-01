package main

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"sync"

	"forge/internal/core/model"
	"forge/internal/logging"
	"forge/internal/mcpserve"
)

// runMCP is `forge mcp --attempt <id>`: the per-attempt MCP server every agent
// process loads over stdio (DESIGN.md §13). It never starts a daemon — the
// worker launches it with FORGE_SOCKET/FORGE_HTTP and FORGE_TOKEN already set,
// and an unreachable daemon is a one-line failure, because auto-starting from
// inside an agent's tool config would race the worker that owns the attempt.
func runMCP(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("mcp")
	attempt := fs.String("attempt", "", "attempt id this server belongs to (required)")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() > 0 {
		fmt.Fprintf(c.stderr, "forge mcp: unexpected argument %q\n", fs.Arg(0))
		return 2
	}
	if *attempt == "" {
		fmt.Fprintln(c.stderr, "forge mcp: --attempt is required")
		return 2
	}
	if err := model.ValidateID(*attempt); err != nil {
		fmt.Fprintln(c.stderr, "forge mcp:", err)
		return 2
	}
	_, log, code := c.resolveLogging(lf, "mcp")
	if code >= 0 {
		return code
	}
	ctx = logging.ContextWith(ctx, slog.String("attempt_id", *attempt))
	client, err := mcpserve.NewClient(c.getenv)
	if err != nil {
		fmt.Fprintln(c.stderr, "forge mcp:", err)
		return 1
	}
	bridge, err := mcpserve.NewBridge(ctx, client, *attempt, log)
	if err != nil {
		fmt.Fprintln(c.stderr, "forge mcp:", err)
		return 1
	}
	log.InfoContext(ctx, "mcp server ready", "tools", len(bridge.Tools()))
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		bridge.RunSender(ctx)
	}()
	// stdout is the MCP stream itself, so it bypasses c.stdout on purpose:
	// nothing else in this process may ever write to it.
	serveErr := srvServe(ctx, bridge)
	bridge.Close()
	wg.Wait()
	if serveErr != nil && !errors.Is(serveErr, context.Canceled) {
		log.ErrorContext(ctx, "mcp server failed", "error", serveErr)
		fmt.Fprintln(c.stderr, "forge mcp:", serveErr)
		return 1
	}
	log.InfoContext(ctx, "mcp server exiting on eof")
	return 0
}

// srvServe runs the transport over this process's stdio; split out so runMCP
// reads as lifecycle only.
func srvServe(ctx context.Context, bridge *mcpserve.Bridge) error {
	srv := &mcpserve.Server{Version: version, Tools: bridge.Tools(), Call: bridge.Call}
	return srv.Serve(ctx, os.Stdin, os.Stdout)
}
