// Command forge is the single binary for the Forge daemon, worker, MCP server,
// and operator CLI. Each subcommand is one file in this package exposing a
// `func(ctx context.Context, c *tui.Context, args []string) int`; main only builds
// the table and dispatches. Every subcommand parses the logging flags through
// tui.Context.flags, so `forge <anything> -vv --log-format json` always works.
package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"forge/internal/core/logging"
	"forge/internal/tui"
)

// version is set by the linker (`-X main.version=...`); "dev" otherwise. It is the
// one package-level variable STYLE.md allows.
var version = "dev"

// command is a subcommand. Exit codes: 0 ok, 1 failed, 2 usage.
type command struct {
	summary string
	run     func(ctx context.Context, c *tui.Context, args []string) int
}

func main() {
	home, err := os.UserHomeDir()
	if err != nil {
		fmt.Fprintln(os.Stderr, "forge: resolve home directory:", err)
		os.Exit(1)
	}
	forgeHome := os.Getenv("FORGE_HOME")
	if forgeHome == "" {
		forgeHome = filepath.Join(home, ".forge")
	}
	c := &tui.Context{Stdin: os.Stdin, Stdout: os.Stdout, Stderr: os.Stderr, Getenv: os.Getenv, ForgeHome: forgeHome, UserHome: home, Now: time.Now, Version: version}
	os.Exit(dispatch(context.Background(), commands(), c, os.Args[1:]))
}

// commands is the table main dispatches on. Adding a subcommand is one file plus one
// entry here.
func commands() map[string]command {
	return map[string]command{
		"version":     {summary: "print the build version", run: runVersion},
		"init":        {summary: "interactive setup: binaries, repositories, kb path, service, browser", run: tui.RunInit},
		"doctor":      {summary: "health checks with fix hints; exit 1 if anything failed", run: tui.RunDoctor},
		"service":     {summary: "install|uninstall|status of the systemd user units", run: tui.RunService},
		"fake-claude": {summary: "replay a recorded stream-json fixture (test executor)", run: runFakeClaude},
		"daemon":      {summary: "start|stop|restart|status|logs|log-level", run: runDaemon},
		"worker":      {summary: "start the worker process", run: runWorker},
		"task":        {summary: "add|list|show|cancel|answer tasks", run: tui.RunTask},
		"repo":        {summary: "list|show|add|archive|restore|pause|resume|cancel|set-app-url repositories", run: tui.RunRepo},
		"persona":     {summary: "list|show the file-backed personas (~/.forge/directives)", run: tui.RunPersona},
		"routine":     {summary: "add|list|show|edit|run|enable|disable routines", run: tui.RunRoutine},
		"workflow":    {summary: "routines strung together: add|list|show|edit|run|runs", run: tui.RunWorkflow},
		"queue":       {summary: "show the priority queue", run: tui.RunQueue},
		"cleanup":     {summary: "preview or remove a retained worktree", run: tui.RunCleanup},
		"kb":          {summary: "new|resolve|backlinks|links|graph|search|check|export notes", run: tui.RunKb},
		"proposal":    {summary: "list, inspect, approve, or reject proposals", run: tui.RunProposal},
		"prune":       {summary: "apply the retention policy to raw output and artifacts", run: tui.RunPrune},
		"usage":       {summary: "budget windows: utilization, rates, forecast, target", run: tui.RunUsage},
		"stats":       {summary: "per-routine outcomes, durations, tokens, cost", run: tui.RunStats},
		"retro":       {summary: "emit the reflection data pack as JSON", run: tui.RunRetro},
		"mcp":         {summary: "per-attempt MCP server over stdio (loaded by the agent)", run: runMCP},
		"plugin":      {summary: "list|install|uninstall|enable|disable|logs|status plugins", run: tui.RunPlugin},
		"backup":      {summary: "write a backup archive via the running daemon", run: tui.RunBackup},
		"restore":     {summary: "restore a backup archive into a fresh FORGE_HOME", run: tui.RunRestore},
		"eval":        {summary: "run golden eval cases on the fake executor and score them", run: tui.RunEval},
		"directives":  {summary: "search the library, or update it from its upstream base repo", run: tui.RunDirectives},
	}
}

func dispatch(ctx context.Context, table map[string]command, c *tui.Context, args []string) int {
	if len(args) == 0 {
		fmt.Fprint(c.Stderr, usage(table))
		return 2
	}
	switch args[0] {
	case "-h", "--help", "help":
		fmt.Fprint(c.Stdout, usage(table))
		return 0
	}
	cmd, ok := table[args[0]]
	if !ok {
		fmt.Fprintf(c.Stderr, "forge: unknown command %q\n%s", args[0], usage(table))
		return 2
	}
	return cmd.run(ctx, c, args[1:])
}

func usage(table map[string]command) string {
	names := make([]string, 0, len(table))
	for name := range table {
		names = append(names, name)
	}
	sort.Strings(names)
	var b strings.Builder
	b.WriteString("usage: forge <command> [flags]\n\ncommands:\n")
	for _, name := range names {
		fmt.Fprintf(&b, "  %-10s %s\n", name, table[name].summary)
	}
	b.WriteString("\nevery command accepts --log-level, --log-format, -v, -vv\n")
	return b.String()
}

func runVersion(ctx context.Context, c *tui.Context, args []string) int {
	fs, lf := c.Flags("version")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() > 0 {
		fmt.Fprintf(c.Stderr, "forge version: unexpected argument %q\n", fs.Arg(0))
		return 2
	}
	_, log, err := c.Logger(lf, logging.Config{}, "cli.version")
	if err != nil {
		fmt.Fprintln(c.Stderr, "forge version:", err)
		return 2
	}
	log.DebugContext(ctx, "printing version", "version", version)
	fmt.Fprintln(c.Stdout, "forge", version)
	return 0
}
