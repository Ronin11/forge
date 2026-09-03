package tui

import (
	"context"
	"fmt"
	"net/http"
	"net/url"
	"strings"
	"text/tabwriter"
	"time"
)

// The persona CLI reads the daemon's loaded prompts library (the files live
// in ~/.forge/prompts — editing is git and your editor, not this command).
// `show --resolved` prints exactly what an agent will read: the one
// affordance that keeps a composed prompt debuggable.

// personaRowView mirrors the API's fragment row.
type personaRowView struct {
	Name    string   `json:"name"`
	Model   string   `json:"model,omitempty"`
	Modes   []string `json:"modes,omitempty"`
	Hash    string   `json:"hash"`
	Persona bool     `json:"persona"`
}

type personaListView struct {
	Commit   string           `json:"commit,omitempty"`
	Dirty    bool             `json:"dirty"`
	LoadedAt time.Time        `json:"loaded_at"`
	Dir      string           `json:"dir"`
	Rows     []personaRowView `json:"fragments"`
}

type personaDetailView struct {
	personaRowView
	Body     string `json:"body,omitempty"`
	Resolved string `json:"resolved,omitempty"`
}

func RunPersona(ctx context.Context, c *Context, args []string) int {
	if len(args) == 0 || strings.HasPrefix(args[0], "-") {
		code := 2
		if len(args) > 0 && (args[0] == "--help" || args[0] == "-h") {
			code = 0
		}
		fmt.Fprintln(c.Stderr, "usage: forge persona list | show NAME [--resolved [--mode M]]")
		fmt.Fprintln(c.Stderr, "personas live in ~/.forge/prompts (git + Markdown); edit the files, the daemon reloads")
		return code
	}
	sub, rest := args[0], args[1:]
	switch sub {
	case "list":
		return runPersonaList(ctx, c, rest)
	case "show":
		return runPersonaShow(ctx, c, rest)
	}
	fmt.Fprintf(c.Stderr, "forge persona: unknown subcommand %q\n", sub)
	return 2
}

func runPersonaList(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("persona list")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.ResolveLogging(lf, "cli.persona")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("persona list", err)
	}
	var out personaListView
	if err := cl.Do(ctx, http.MethodGet, "/api/v1/personas", nil, &out); err != nil {
		return c.Fail("persona list", err)
	}
	if *asJSON {
		c.PrintJSON(out)
		return 0
	}
	state := "clean"
	if out.Dirty {
		state = "dirty"
	}
	commit := out.Commit
	if commit == "" {
		commit = "(not a git repo)"
	}
	fmt.Fprintf(c.Stdout, "%s @ %s (%s)\n", out.Dir, short(commit), state)
	tw := tabwriter.NewWriter(c.Stdout, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "NAME\tKIND\tMODEL\tMODES")
	for _, row := range out.Rows {
		kind := "fragment"
		if row.Persona {
			kind = "persona"
		}
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", row.Name, kind, row.Model, strings.Join(row.Modes, " "))
	}
	if err := tw.Flush(); err != nil {
		return c.Fail("persona list", err)
	}
	return 0
}

func runPersonaShow(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("persona show")
	asJSON := fs.Bool("json", false, "JSON output")
	resolved := fs.Bool("resolved", false, "print the fully composed text an agent will read")
	mode := fs.String("mode", "", "compose this mode's section (with --resolved)")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.Stderr, "usage: forge persona show NAME [--resolved [--mode M]]")
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.persona")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("persona show", err)
	}
	q := ""
	if *resolved {
		q = "?resolved=1&mode=" + url.QueryEscape(*mode)
	}
	var out personaDetailView
	if err := cl.Do(ctx, http.MethodGet, "/api/v1/personas/"+url.PathEscape(fs.Arg(0))+q, nil, &out); err != nil {
		return c.Fail("persona show", err)
	}
	if *asJSON {
		c.PrintJSON(out)
		return 0
	}
	if *resolved {
		fmt.Fprintln(c.Stdout, out.Resolved)
		return 0
	}
	fmt.Fprintln(c.Stdout, out.Body)
	return 0
}
