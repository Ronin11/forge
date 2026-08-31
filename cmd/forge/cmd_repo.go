package main

import (
	"context"
	"fmt"
	"net/http"
	"strings"
	"text/tabwriter"

	"forge/internal/model"
	"forge/internal/store"
)

// repoRow is one row of GET /api/v1/repositories: the repository plus its
// derived state.
type repoRow struct {
	store.Repository
	State string `json:"state"`
}

// repoWork is one task summary in a repository's detail (running or recent).
type repoWork struct {
	Work    store.Work      `json:"work"`
	State   model.WorkState `json:"state"`
	Targets []store.Target  `json:"targets"`
}

// repoDetailView is GET /api/v1/repositories/{name}.
type repoDetailView struct {
	Repository    store.Repository `json:"repository"`
	State         string           `json:"state"`
	Running       []repoWork       `json:"running"`
	Recent        []repoWork       `json:"recent"`
	RetainedCount int              `json:"retained_count"`
	Retained      []struct {
		AttemptID      string `json:"attempt_id"`
		Path           string `json:"path"`
		Reason         string `json:"reason"`
		CleanupCommand string `json:"cleanup_command"`
	} `json:"retained"`
	Checks []string `json:"checks"`
	Paused bool     `json:"paused"`
	AppURL string   `json:"app_url"`
}

func runRepo(ctx context.Context, c *cmdContext, args []string) int {
	if len(args) == 0 || strings.HasPrefix(args[0], "-") {
		code := 2
		if len(args) > 0 && (args[0] == "--help" || args[0] == "-h") {
			code = 0
		}
		fmt.Fprintln(c.stderr, "usage: forge repo list|show|pause|resume|cancel|set-app-url [flags]")
		return code
	}
	switch args[0] {
	case "list":
		return runRepoList(ctx, c, args[1:])
	case "show":
		return runRepoShow(ctx, c, args[1:])
	case "pause":
		return runRepoAction(ctx, c, args[1:], "pause")
	case "resume":
		return runRepoAction(ctx, c, args[1:], "resume")
	case "cancel":
		return runRepoAction(ctx, c, args[1:], "cancel-running")
	case "set-app-url":
		return runRepoSetAppURL(ctx, c, args[1:])
	}
	fmt.Fprintf(c.stderr, "forge repo: unknown subcommand %q\n", args[0])
	return 2
}

func runRepoList(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("repo list")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.resolveLogging(lf, "cli.repo")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("repo list", err)
	}
	var out []repoRow
	if err := cl.do(ctx, http.MethodGet, "/api/v1/repositories", nil, &out); err != nil {
		return c.fail("repo list", err)
	}
	if *asJSON {
		c.printJSON(out)
		return 0
	}
	tw := tabwriter.NewWriter(c.stdout, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "NAME\tSTATE\tPAUSED\tPATH\tORIGIN")
	for _, r := range out {
		fmt.Fprintf(tw, "%s\t%s\t%v\t%s\t%s\n", r.Name, r.State, r.Paused, r.Path, r.OriginIdentity)
	}
	if err := tw.Flush(); err != nil {
		return c.fail("repo list", err)
	}
	return 0
}

func runRepoShow(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("repo show")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.stderr, "usage: forge repo show NAME")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.repo")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("repo show", err)
	}
	var v repoDetailView
	if err := cl.do(ctx, http.MethodGet, "/api/v1/repositories/"+fs.Arg(0), nil, &v); err != nil {
		return c.fail("repo show", err)
	}
	if *asJSON {
		c.printJSON(v)
		return 0
	}
	fmt.Fprintf(c.stdout, "repository %s  %s  paused %v\n", v.Repository.Name, v.State, v.Paused)
	fmt.Fprintf(c.stdout, "path: %s\n", v.Repository.Path)
	fmt.Fprintf(c.stdout, "origin: %s\n", v.Repository.OriginIdentity)
	if v.AppURL != "" {
		fmt.Fprintf(c.stdout, "app: %s\n", v.AppURL)
	}
	if len(v.Checks) > 0 {
		fmt.Fprintf(c.stdout, "checks: %s\n", strings.Join(v.Checks, ", "))
	}
	fmt.Fprintf(c.stdout, "retained worktrees: %d\n", v.RetainedCount)
	for _, w := range v.Running {
		fmt.Fprintf(c.stdout, "  running %s  %s  %s\n", short(w.Work.ID), w.State, w.Work.Title)
	}
	for _, w := range v.Recent {
		fmt.Fprintf(c.stdout, "  recent  %s  %s  %s\n", short(w.Work.ID), w.State, w.Work.Title)
	}
	return 0
}

// runRepoAction posts one of the no-body repository controls and prints the
// outcome. cancel-running answers with a count; pause/resume with the repo.
func runRepoAction(ctx context.Context, c *cmdContext, args []string, action string) int {
	fs, lf := c.flags("repo " + action)
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintf(c.stderr, "usage: forge repo %s NAME\n", strings.TrimSuffix(action, "-running"))
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.repo")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("repo "+action, err)
	}
	name := fs.Arg(0)
	path := "/api/v1/repositories/" + name + "/" + action
	if action == "cancel-running" {
		var out struct {
			Cancelled int `json:"cancelled"`
		}
		if err := cl.do(ctx, http.MethodPost, path, nil, &out); err != nil {
			return c.fail("repo cancel", err)
		}
		fmt.Fprintf(c.stdout, "repository %s: cancelled %d running task(s)\n", name, out.Cancelled)
		return 0
	}
	var repo store.Repository
	if err := cl.do(ctx, http.MethodPost, path, nil, &repo); err != nil {
		return c.fail("repo "+action, err)
	}
	fmt.Fprintf(c.stdout, "repository %s: paused %v\n", repo.Name, repo.Paused)
	return 0
}

func runRepoSetAppURL(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("repo set-app-url")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() < 1 {
		fmt.Fprintln(c.stderr, "usage: forge repo set-app-url NAME [URL]  (omit URL to clear)")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.repo")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("repo set-app-url", err)
	}
	name := fs.Arg(0)
	url := strings.Join(fs.Args()[1:], " ")
	var repo store.Repository
	if err := cl.do(ctx, http.MethodPost, "/api/v1/repositories/"+name+"/app-url", map[string]string{"url": url}, &repo); err != nil {
		return c.fail("repo set-app-url", err)
	}
	if repo.AppURL == "" {
		fmt.Fprintf(c.stdout, "repository %s: app url cleared\n", repo.Name)
	} else {
		fmt.Fprintf(c.stdout, "repository %s: app url set to %s\n", repo.Name, repo.AppURL)
	}
	return 0
}
