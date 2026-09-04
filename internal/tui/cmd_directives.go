package tui

import (
	"context"
	"fmt"
	"net/http"
	"net/url"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"text/tabwriter"

	"forge/internal/core/config"
)

// RunDirectives is `forge directives update`: pull base-library improvements
// from the upstream remote into the user's fork of ~/.forge/directives. The
// merge is an ordinary git merge — a conflict is left for the user to resolve
// in the library repo like any other; the daemon keeps serving its last good
// load throughout and picks the merged tree up within 30s.
func RunDirectives(ctx context.Context, c *Context, args []string) int {
	if len(args) == 0 {
		fmt.Fprintln(c.Stderr, "usage: forge directives update | search QUERY [--kind k]")
		return 2
	}
	if args[0] == "search" {
		return runDirectivesSearch(ctx, c, args[1:])
	}
	if args[0] != "update" {
		fmt.Fprintln(c.Stderr, "usage: forge directives update | search QUERY [--kind k]")
		return 2
	}
	fs, _ := c.Flags("directives update")
	if code := c.Parse(fs, args[1:]); code >= 0 {
		return code
	}
	cfg, err := config.LoadConfig(filepath.Join(c.ForgeHome, "config.toml"), c.ForgeHome, c.UserHome, c.Getenv)
	if err != nil {
		return c.Fail("directives", err)
	}
	dir := cfg.Directives.Path
	git := func(args ...string) (string, error) {
		out, err := exec.CommandContext(ctx, "git", append([]string{"-C", dir}, args...)...).CombinedOutput()
		return strings.TrimSpace(string(out)), err
	}
	if out, err := git("remote", "get-url", "upstream"); err != nil {
		return c.Fail("directives update", fmt.Errorf("no upstream remote in %s (%s) — add one: git -C %s remote add upstream <url>", dir, out, dir))
	}
	if out, err := git("fetch", "upstream"); err != nil {
		return c.Fail("directives update", fmt.Errorf("fetch: %s", out))
	}
	out, err := git("merge", "--no-edit", "FETCH_HEAD")
	if err != nil {
		fmt.Fprintln(c.Stdout, out)
		return c.Fail("directives update", fmt.Errorf("merge conflict — resolve it in %s (git status there), then commit; the daemon keeps its last good load meanwhile", dir))
	}
	fmt.Fprintln(c.Stdout, out)
	fmt.Fprintln(c.Stdout, "updated — the daemon reloads within 30s; new seeds import on its next boot")
	return 0
}

// runDirectivesSearch queries the daemon's library search: one ranking for
// the UI, the CLI, and the forge_library agent tool.
func runDirectivesSearch(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("directives search")
	kind := fs.String("kind", "", "directive|persona|fragment|script|workflow (comma-separated)")
	limit := fs.Int("limit", 20, "max results")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.ResolveLogging(lf, "cli.directives")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("directives search", err)
	}
	q := url.Values{"q": {strings.Join(fs.Args(), " ")}, "limit": {strconv.Itoa(*limit)}}
	if *kind != "" {
		q.Set("kind", *kind)
	}
	var out struct {
		Hits []struct {
			Name        string `json:"name"`
			Kind        string `json:"kind"`
			Description string `json:"description"`
			Tool        bool   `json:"tool"`
		} `json:"hits"`
	}
	if err := cl.Do(ctx, http.MethodGet, "/api/v1/library/search?"+q.Encode(), nil, &out); err != nil {
		return c.Fail("directives search", err)
	}
	if len(out.Hits) == 0 {
		fmt.Fprintln(c.Stdout, "no matches")
		return 0
	}
	w := tabwriter.NewWriter(c.Stdout, 2, 4, 2, ' ', 0)
	fmt.Fprintln(w, "KIND\tNAME\tTOOL\tDESCRIPTION")
	for _, h := range out.Hits {
		tool := ""
		if h.Tool {
			tool = "yes"
		}
		fmt.Fprintf(w, "%s\t%s\t%s\t%s\n", h.Kind, h.Name, tool, h.Description)
	}
	if err := w.Flush(); err != nil {
		return c.Fail("directives search", err)
	}
	return 0
}
