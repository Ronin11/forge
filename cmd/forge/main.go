// Command forge is the single binary for the Forge daemon, worker, MCP server,
// and operator CLI. Each subcommand is one file in this package exposing a
// `func(ctx context.Context, c *cmdContext, args []string) int`; main only builds
// the table and dispatches. Every subcommand parses the logging flags through
// cmdContext.flags, so `forge <anything> -vv --log-format json` always works.
package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"forge/internal/logging"
)

// version is set by the linker (`-X main.version=...`); "dev" otherwise. It is the
// one package-level variable STYLE.md allows.
var version = "dev"

// command is a subcommand. Exit codes: 0 ok, 1 failed, 2 usage.
type command struct {
	summary string
	run     func(ctx context.Context, c *cmdContext, args []string) int
}

// cmdContext is what a subcommand gets from main: streams, environment, the Forge
// home directory, and the means to build its logger once its flags are parsed.
type cmdContext struct {
	stdin          io.Reader // prompts (forge init); nil takes every default
	stdout, stderr io.Writer
	getenv         func(string) string
	forgeHome      string // ~/.forge, or $FORGE_HOME
	userHome       string
	now            func() time.Time
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
	c := &cmdContext{stdin: os.Stdin, stdout: os.Stdout, stderr: os.Stderr, getenv: os.Getenv, forgeHome: forgeHome, userHome: home, now: time.Now}
	os.Exit(dispatch(context.Background(), commands(), c, os.Args[1:]))
}

// commands is the table main dispatches on. Adding a subcommand is one file plus one
// entry here.
func commands() map[string]command {
	return map[string]command{
		"version":     {summary: "print the build version", run: runVersion},
		"init":        {summary: "interactive setup: binaries, repositories, kb path, service, browser", run: runInit},
		"doctor":      {summary: "health checks with fix hints; exit 1 if anything failed", run: runDoctor},
		"service":     {summary: "install|uninstall|status of the systemd user units", run: runService},
		"fake-claude": {summary: "replay a recorded stream-json fixture (test executor)", run: runFakeClaude},
		"daemon":      {summary: "start|stop|restart|status|logs|log-level", run: runDaemon},
		"worker":      {summary: "start the worker process", run: runWorker},
		"task":        {summary: "add|list|show|cancel|answer tasks", run: runTask},
		"repo":        {summary: "list|show|add|archive|restore|pause|resume|cancel|set-app-url repositories", run: runRepo},
		"routine":     {summary: "add|list|show|edit|run|enable|disable routines", run: runRoutine},
		"workflow":    {summary: "routines strung together: add|list|show|edit|run|runs", run: runWorkflow},
		"queue":       {summary: "show the priority queue", run: runQueue},
		"cleanup":     {summary: "preview or remove a retained worktree", run: runCleanup},
		"kb":          {summary: "new|resolve|backlinks|links|graph|search|check|export notes", run: runKb},
		"proposal":    {summary: "list, inspect, approve, or reject proposals", run: runProposal},
		"prune":       {summary: "apply the retention policy to raw output and artifacts", run: runPrune},
		"usage":       {summary: "budget windows: utilization, rates, forecast, target", run: runUsage},
		"stats":       {summary: "per-routine outcomes, durations, tokens, cost", run: runStats},
		"retro":       {summary: "emit the reflection data pack as JSON", run: runRetro},
		"mcp":         {summary: "per-attempt MCP server over stdio (loaded by the agent)", run: runMCP},
		"plugin":      {summary: "list|install|uninstall|enable|disable|logs|status plugins", run: runPlugin},
		"backup":      {summary: "write a backup archive via the running daemon", run: runBackup},
		"restore":     {summary: "restore a backup archive into a fresh FORGE_HOME", run: runRestore},
		"eval":        {summary: "run golden eval cases on the fake executor and score them", run: runEval},
	}
}

func dispatch(ctx context.Context, table map[string]command, c *cmdContext, args []string) int {
	if len(args) == 0 {
		fmt.Fprint(c.stderr, usage(table))
		return 2
	}
	switch args[0] {
	case "-h", "--help", "help":
		fmt.Fprint(c.stdout, usage(table))
		return 0
	}
	cmd, ok := table[args[0]]
	if !ok {
		fmt.Fprintf(c.stderr, "forge: unknown command %q\n%s", args[0], usage(table))
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

// flags builds a subcommand's FlagSet with the logging flags already registered.
// Parse errors print to stderr; -h prints usage to stdout (see parse).
func (c *cmdContext) flags(name string) (*flag.FlagSet, *logging.Flags) {
	fs := flag.NewFlagSet("forge "+name, flag.ContinueOnError)
	fs.SetOutput(c.stderr)
	return fs, logging.AddFlags(fs)
}

// parse runs fs.Parse and maps its outcome to an exit code: -1 means "carry on".
// Only an explicit -h moves usage to stdout; a bad flag keeps everything on stderr
// so a script piping stdout never sees help text where it expected output.
func (c *cmdContext) parse(fs *flag.FlagSet, args []string) int {
	fs.Usage = func() {}
	switch err := fs.Parse(flagsFirst(fs, args)); {
	case err == flag.ErrHelp:
		fmt.Fprintf(c.stdout, "usage: %s [flags]\n", fs.Name())
		fs.SetOutput(c.stdout)
		fs.PrintDefaults()
		return 0
	case err != nil:
		fmt.Fprintln(c.stderr, fs.Name()+":", err)
		return 2
	}
	return -1
}

// logger resolves the logging options (flag > env > config) and returns the
// handler and this command's own logger. One-shot commands have no [log] config
// yet; the daemon and worker pass theirs in when their config loads (M1).
func (c *cmdContext) logger(f *logging.Flags, cfg logging.Config, component string) (*logging.Handler, *slog.Logger, error) {
	opts, err := logging.Resolve(f, c.getenv, cfg, c.forgeHome)
	if err != nil {
		return nil, nil, err
	}
	h := logging.New(c.stderr, opts, nil)
	return h, h.For(component), nil
}

func runVersion(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("version")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() > 0 {
		fmt.Fprintf(c.stderr, "forge version: unexpected argument %q\n", fs.Arg(0))
		return 2
	}
	_, log, err := c.logger(lf, logging.Config{}, "cli.version")
	if err != nil {
		fmt.Fprintln(c.stderr, "forge version:", err)
		return 2
	}
	log.DebugContext(ctx, "printing version", "version", version)
	fmt.Fprintln(c.stdout, "forge", version)
	return 0
}

// flagsFirst reorders args so every flag precedes the positionals: `forge
// routine add NAME --prompt …` reads naturally, but the flag package stops at
// the first positional. A flag's value stays attached to it; "--" ends flags.
func flagsFirst(fs *flag.FlagSet, args []string) []string {
	var flags, positional []string
	for i := 0; i < len(args); i++ {
		a := args[i]
		if a == "--" {
			positional = append(positional, args[i+1:]...)
			break
		}
		if !strings.HasPrefix(a, "-") || a == "-" {
			positional = append(positional, a)
			continue
		}
		flags = append(flags, a)
		name, _, hasValue := strings.Cut(strings.TrimLeft(a, "-"), "=")
		if hasValue {
			continue
		}
		f := fs.Lookup(name)
		isBool := false
		if f != nil {
			if b, ok := f.Value.(interface{ IsBoolFlag() bool }); ok && b.IsBoolFlag() {
				isBool = true
			}
		}
		if !isBool && i+1 < len(args) {
			flags = append(flags, args[i+1])
			i++
		}
	}
	return append(flags, positional...)
}
