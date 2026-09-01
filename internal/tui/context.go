// Package tui is the operator CLI's client half (MODULARIZATION.md §6): the
// command context, the HTTP client over the daemon's socket, and the
// operator-facing subcommands. cmd/forge stays the composition root — it
// builds the Context and dispatches. A future terminal UI lives beside these,
// reusing the client.
package tui

import (
	"flag"
	"fmt"
	"io"
	"log/slog"
	"strings"
	"time"

	"forge/internal/core/logging"
)

// Context is what a subcommand gets from main: streams, environment, the
// Forge home directory, and the means to build its logger once its flags are
// parsed.
type Context struct {
	Stdin          io.Reader // prompts (forge init); nil takes every default
	Stdout, Stderr io.Writer
	Getenv         func(string) string
	ForgeHome      string // ~/.forge, or $FORGE_HOME
	UserHome       string
	Now            func() time.Time
	// Version is the build version main stamps in; the client handshake
	// compares it against the daemon's.
	Version string
}

// flags builds a subcommand's FlagSet with the logging flags already registered.
// Parse errors print to stderr; -h prints usage to stdout (see parse).
func (c *Context) Flags(name string) (*flag.FlagSet, *logging.Flags) {
	fs := flag.NewFlagSet("forge "+name, flag.ContinueOnError)
	fs.SetOutput(c.Stderr)
	return fs, logging.AddFlags(fs)
}

// parse runs fs.Parse and maps its outcome to an exit code: -1 means "carry on".
// Only an explicit -h moves usage to stdout; a bad flag keeps everything on stderr
// so a script piping stdout never sees help text where it expected output.
func (c *Context) Parse(fs *flag.FlagSet, args []string) int {
	fs.Usage = func() {}
	switch err := fs.Parse(flagsFirst(fs, args)); {
	case err == flag.ErrHelp:
		fmt.Fprintf(c.Stdout, "usage: %s [flags]\n", fs.Name())
		fs.SetOutput(c.Stdout)
		fs.PrintDefaults()
		return 0
	case err != nil:
		fmt.Fprintln(c.Stderr, fs.Name()+":", err)
		return 2
	}
	return -1
}

// logger resolves the logging options (flag > env > config) and returns the
// handler and this command's own logger. One-shot commands have no [log] config
// yet; the daemon and worker pass theirs in when their config loads (M1).
func (c *Context) Logger(f *logging.Flags, cfg logging.Config, component string) (*logging.Handler, *slog.Logger, error) {
	opts, err := logging.Resolve(f, c.Getenv, cfg, c.ForgeHome)
	if err != nil {
		return nil, nil, err
	}
	h := logging.New(c.Stderr, opts, nil)
	return h, h.For(component), nil
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
