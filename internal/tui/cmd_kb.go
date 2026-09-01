package tui

import (
	"context"
	"fmt"
	"net/http"
	"net/url"
	"path/filepath"
	"strings"
	"text/tabwriter"

	"forge/internal/core/kb"
	"forge/internal/core/store"
)

// kbDir is where notes live; the daemon's config may override, but the CLI and
// daemon agree on the default so file-local commands work with the daemon down.
func (c *Context) kbDir() string { return filepath.Join(c.ForgeHome, "kb") }

func RunKb(ctx context.Context, c *Context, args []string) int {
	if len(args) == 0 || strings.HasPrefix(args[0], "-") {
		// --help on a parent command is a request, not a mistake.
		code := 2
		if len(args) > 0 && (args[0] == "--help" || args[0] == "-h") {
			code = 0
		}
		fmt.Fprintln(c.Stderr, "usage: forge kb new|resolve|backlinks|links|graph|search|check|export [flags]")
		return code
	}
	sub, rest := args[0], args[1:]
	switch sub {
	case "new":
		return runKbNew(ctx, c, rest)
	case "check":
		return runKbCheck(ctx, c, rest)
	case "export":
		return runKbExport(ctx, c, rest)
	case "resolve", "graph":
		return runKbLocal(ctx, c, sub, rest)
	case "search", "backlinks", "links":
		return runKbIndexed(ctx, c, sub, rest)
	}
	fmt.Fprintf(c.Stderr, "forge kb: unknown subcommand %q\n", sub)
	return 2
}

func runKbNew(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("kb new")
	title := fs.String("title", "", "note title (required)")
	typ := fs.String("type", "note", "retro|hypothesis|proposal|spec|note")
	tags := fs.String("tags", "", "comma-separated tags")
	body := fs.String("body", "", "note body (default: a stub)")
	var about, supersedes, evidence multiFlag
	fs.Var(&about, "about", "link target: a note id or fact link like attempt:<id> (repeatable)")
	fs.Var(&supersedes, "supersedes", "note this one supersedes (repeatable)")
	fs.Var(&evidence, "evidence-for", "note this one is evidence for (repeatable)")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if *title == "" {
		fmt.Fprintln(c.Stderr, "forge kb new: --title is required")
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.kb")
	if code >= 0 {
		return code
	}
	in := kb.New{Title: *title, Type: *typ, Body: *body, Links: map[string][]string{}}
	if *tags != "" {
		in.Tags = strings.Split(*tags, ",")
	}
	for lt, v := range map[string][]string{"about": about, "supersedes": supersedes, "evidence_for": evidence} {
		if len(v) > 0 {
			in.Links[lt] = v
		}
	}
	if in.Body == "" {
		in.Body = "(empty)\n"
	}
	n, err := kb.WriteNew(c.kbDir(), in, c.Now())
	if err != nil {
		return c.Fail("kb new", err)
	}
	fmt.Fprintf(c.Stdout, "%s\t%s\n", n.ID, n.Path)
	// Best effort: tell a running daemon to reindex now rather than on its timer.
	cl := c.Client(log)
	cl.NoAutoStart = true
	if err := cl.Connect(ctx); err == nil {
		if err := cl.Do(ctx, http.MethodPost, "/api/v1/kb/reindex", nil, nil); err != nil {
			log.DebugContext(ctx, "reindex ping failed", "error", err)
		}
	}
	return 0
}

func runKbCheck(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("kb check")
	path := fs.String("path", c.kbDir(), "kb directory")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.ResolveLogging(lf, "cli.kb")
	if code >= 0 {
		return code
	}
	// Fact links are verified through a running daemon; with none, they are
	// warnings — `just check` must pass offline.
	var facts kb.FactChecker
	cl := c.Client(log)
	cl.NoAutoStart = true
	if err := cl.Connect(ctx); err == nil {
		facts = func(ref kb.Ref) (bool, error) {
			var out struct {
				Exists bool `json:"exists"`
			}
			err := cl.Do(ctx, http.MethodGet, "/api/v1/kb/resolve-fact?ref="+url.QueryEscape(ref.Kind+":"+ref.Val), nil, &out)
			return out.Exists, err
		}
	}
	res, err := kb.Check(*path, facts)
	if err != nil {
		return c.Fail("kb check", err)
	}
	for _, w := range res.Warnings {
		fmt.Fprintln(c.Stderr, "warning:", w)
	}
	for _, f := range res.Findings {
		fmt.Fprintf(c.Stdout, "%s: %s\n", f.Path, f.Problem)
	}
	if len(res.Findings) > 0 {
		fmt.Fprintf(c.Stderr, "kb check: %d problem(s) in %d note(s)\n", len(res.Findings), res.Notes)
		return 1
	}
	fmt.Fprintf(c.Stdout, "kb check: %d note(s) ok\n", res.Notes)
	return 0
}

func runKbExport(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("kb export")
	path := fs.String("path", c.kbDir(), "kb directory")
	out := fs.String("out", "", "output directory (required)")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if *out == "" {
		fmt.Fprintln(c.Stderr, "forge kb export: --out is required")
		return 2
	}
	if _, _, code := c.ResolveLogging(lf, "cli.kb"); code >= 0 {
		return code
	}
	n, err := kb.Export(*path, *out)
	if err != nil {
		return c.Fail("kb export", err)
	}
	fmt.Fprintf(c.Stdout, "exported %d note(s) to %s\n", n, *out)
	return 0
}

// runKbLocal serves resolve and graph from the files alone.
func runKbLocal(ctx context.Context, c *Context, sub string, args []string) int {
	fs, lf := c.Flags("kb " + sub)
	path := fs.String("path", c.kbDir(), "kb directory")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if _, _, code := c.ResolveLogging(lf, "cli.kb"); code >= 0 {
		return code
	}
	notes, findings, err := kb.Scan(*path)
	if err != nil {
		return c.Fail("kb "+sub, err)
	}
	for _, f := range findings {
		fmt.Fprintf(c.Stderr, "warning: %s: %s\n", f.Path, f.Problem)
	}
	switch sub {
	case "resolve":
		if fs.NArg() != 1 {
			fmt.Fprintln(c.Stderr, "usage: forge kb resolve ID")
			return 2
		}
		for _, n := range notes {
			if n.ID == fs.Arg(0) {
				fmt.Fprintln(c.Stdout, n.Path)
				return 0
			}
		}
		fmt.Fprintf(c.Stderr, "forge kb resolve: no note %q\n", fs.Arg(0))
		return 1
	case "graph":
		for _, n := range notes {
			targets := append([]string(nil), n.Inline...)
			for _, lt := range kb.LinkTypes {
				targets = append(targets, n.Links[lt]...)
			}
			for _, t := range targets {
				fmt.Fprintf(c.Stdout, "%s -> %s\n", n.ID, t)
			}
		}
		return 0
	}
	return 2
}

// runKbIndexed serves search/backlinks/links from the daemon's index.
func runKbIndexed(ctx context.Context, c *Context, sub string, args []string) int {
	fs, lf := c.Flags("kb " + sub)
	limit := fs.Int("limit", 20, "results")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintf(c.Stderr, "usage: forge kb %s %s\n", sub, map[string]string{"search": `"query"`, "backlinks": "REF", "links": "ID"}[sub])
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.kb")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("kb "+sub, err)
	}
	arg := url.QueryEscape(fs.Arg(0))
	switch sub {
	case "search":
		var out []store.KbNote
		if err := cl.Do(ctx, http.MethodGet, fmt.Sprintf("/api/v1/kb/search?q=%s&limit=%d", arg, *limit), nil, &out); err != nil {
			return c.Fail("kb search", err)
		}
		if *asJSON {
			c.PrintJSON(out)
			return 0
		}
		tw := tabwriter.NewWriter(c.Stdout, 0, 4, 2, ' ', 0)
		fmt.Fprintln(tw, "ID\tTYPE\tTITLE\tTAGS")
		for _, n := range out {
			fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", n.ID, n.Type, n.Title, strings.Join(n.Tags, ","))
		}
		if err := tw.Flush(); err != nil {
			return c.Fail("kb search", err)
		}
		return 0
	case "backlinks", "links":
		endpoint := "/api/v1/kb/backlinks?ref=" + arg
		if sub == "links" {
			endpoint = "/api/v1/kb/links?id=" + arg
		}
		var out []store.KbLink
		if err := cl.Do(ctx, http.MethodGet, endpoint, nil, &out); err != nil {
			return c.Fail("kb "+sub, err)
		}
		if *asJSON {
			c.PrintJSON(out)
			return 0
		}
		for _, l := range out {
			fmt.Fprintf(c.Stdout, "%s\t%s\t%s:%s\n", l.FromID, l.LinkType, l.ToKind, l.ToRef)
		}
		return 0
	}
	return 2
}
