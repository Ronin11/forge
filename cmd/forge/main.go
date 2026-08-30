// Command forge is the single binary for the Forge control plane, worker, MCP
// server, and operator CLI. Each subcommand is one file in this package exposing a
// `func(args []string, stdout, stderr io.Writer) int`; main only builds the table
// and dispatches.
package main

import (
	"fmt"
	"io"
	"os"
	"sort"
	"strings"
)

// version is set by the linker (`-X main.version=...`); "dev" otherwise. It is the
// one package-level variable STYLE.md allows.
var version = "dev"

// command is a subcommand. Exit codes: 0 ok, 1 failed, 2 usage.
type command struct {
	summary string
	run     func(args []string, stdout, stderr io.Writer) int
}

func main() {
	os.Exit(dispatch(commands(), os.Args[1:], os.Stdout, os.Stderr))
}

// commands is the table main dispatches on. Adding a subcommand is one file plus one
// entry here.
func commands() map[string]command {
	return map[string]command{
		"version": {summary: "print the build version", run: runVersion},
	}
}

func dispatch(table map[string]command, args []string, stdout, stderr io.Writer) int {
	if len(args) == 0 {
		fmt.Fprint(stderr, usage(table))
		return 2
	}
	switch args[0] {
	case "-h", "--help", "help":
		fmt.Fprint(stdout, usage(table))
		return 0
	}
	cmd, ok := table[args[0]]
	if !ok {
		fmt.Fprintf(stderr, "forge: unknown command %q\n%s", args[0], usage(table))
		return 2
	}
	return cmd.run(args[1:], stdout, stderr)
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
	return b.String()
}

func runVersion(args []string, stdout, stderr io.Writer) int {
	if len(args) > 0 {
		if args[0] == "-h" || args[0] == "--help" {
			fmt.Fprintln(stdout, "usage: forge version\n\nPrint the build version.")
			return 0
		}
		fmt.Fprintf(stderr, "forge version: unexpected argument %q\n", args[0])
		return 2
	}
	fmt.Fprintln(stdout, "forge", version)
	return 0
}
