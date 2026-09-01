package tui

import (
	"context"
	"fmt"
	"net/http"
	"strings"
	"text/tabwriter"

	"github.com/BurntSushi/toml"

	"forge/internal/core/store"
)

func RunWorkflow(ctx context.Context, c *Context, args []string) int {
	if len(args) == 0 || strings.HasPrefix(args[0], "-") {
		// --help on a parent command is a request, not a mistake.
		code := 2
		if len(args) > 0 && (args[0] == "--help" || args[0] == "-h") {
			code = 0
		}
		fmt.Fprintln(c.Stderr, "usage: forge workflow add|list|show|edit|run|runs|enable|disable NAME [flags]")
		return code
	}
	sub, rest := args[0], args[1:]
	switch sub {
	case "add":
		return runWorkflowAdd(ctx, c, rest)
	case "list":
		return runWorkflowList(ctx, c, rest)
	case "show", "run", "runs", "enable", "disable", "edit":
		return runWorkflowNamed(ctx, c, sub, rest)
	}
	fmt.Fprintf(c.Stderr, "forge workflow: unknown subcommand %q\n", sub)
	return 2
}

func runWorkflowAdd(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("workflow add")
	var steps multiFlag
	fs.Var(&steps, "step", "NAME=ROUTINE, repeatable; steps chain in order (use --from for a DAG)")
	from := fs.String("from", "", "TOML file with the full workflow (steps, schedule)")
	schedule := fs.String("schedule", "", "cron schedule")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.Stderr, "usage: forge workflow add NAME --step lint=lint-all --step fix=fix-lint [flags]")
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.workflow")
	if code >= 0 {
		return code
	}
	wf := store.Workflow{Name: fs.Arg(0)}
	if *from != "" {
		if _, err := toml.DecodeFile(*from, &wf); err != nil {
			return c.Fail("workflow add", err)
		}
	}
	for _, s := range steps {
		name, routine, ok := strings.Cut(s, "=")
		if !ok {
			fmt.Fprintf(c.Stderr, "forge workflow add: --step %q: want NAME=ROUTINE\n", s)
			return 2
		}
		wf.Steps = append(wf.Steps, store.WorkflowStep{Name: name, Routine: routine})
	}
	wf.Name = fs.Arg(0)
	if *schedule != "" {
		wf.Schedule = *schedule
	}
	wf.ScheduleEnabled = wf.Schedule != ""
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("workflow add", err)
	}
	var out store.Workflow
	if err := cl.Do(ctx, http.MethodPost, "/api/v1/workflows", wf, &out); err != nil {
		return c.Fail("workflow add", err)
	}
	fmt.Fprintf(c.Stdout, "workflow %s created with %d step(s) (generation %d)\n", out.Name, len(out.Steps), out.Generation)
	return 0
}

func runWorkflowList(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("workflow list")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.ResolveLogging(lf, "cli.workflow")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("workflow list", err)
	}
	var out []store.Workflow
	if err := cl.Do(ctx, http.MethodGet, "/api/v1/workflows", nil, &out); err != nil {
		return c.Fail("workflow list", err)
	}
	if *asJSON {
		c.PrintJSON(out)
		return 0
	}
	tw := tabwriter.NewWriter(c.Stdout, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "NAME\tGEN\tSTEPS\tSCHEDULE")
	for _, wf := range out {
		names := make([]string, len(wf.Steps))
		for i, st := range wf.Steps {
			names[i] = st.Name
		}
		sched := wf.Schedule
		if sched != "" && !wf.ScheduleEnabled {
			sched += " (off)"
		}
		fmt.Fprintf(tw, "%s\t%d\t%s\t%s\n", wf.Name, wf.Generation, strings.Join(names, " → "), sched)
	}
	if err := tw.Flush(); err != nil {
		return c.Fail("workflow list", err)
	}
	return 0
}

// workflowRunView is POST /api/v1/workflows/{name}/run's body.
type workflowRunView struct {
	RunID    string     `json:"run_id"`
	Workflow string     `json:"workflow"`
	Works    []taskView `json:"works"`
}

// workflowRunRow is one row of GET /api/v1/workflows/{name}/runs.
type workflowRunRow struct {
	RunID     string     `json:"run_id"`
	State     string     `json:"state"`
	CreatedAt string     `json:"created_at"`
	Works     []taskView `json:"works"`
}

func runWorkflowNamed(ctx context.Context, c *Context, sub string, args []string) int {
	fs, lf := c.Flags("workflow " + sub)
	asJSON := fs.Bool("json", false, "JSON output")
	var repos multiFlag
	fs.Var(&repos, "repo", "narrow a run to these repositories (run only)")
	from := fs.String("from", "", "apply a TOML file instead of $EDITOR (edit only)")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintf(c.Stderr, "usage: forge workflow %s NAME\n", sub)
		return 2
	}
	name := fs.Arg(0)
	_, log, code := c.ResolveLogging(lf, "cli.workflow")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("workflow "+sub, err)
	}
	var wf store.Workflow
	if err := cl.Do(ctx, http.MethodGet, "/api/v1/workflows/"+name, nil, &wf); err != nil {
		return c.Fail("workflow "+sub, err)
	}
	switch sub {
	case "show":
		if *asJSON {
			c.PrintJSON(wf)
			return 0
		}
		var b strings.Builder
		if err := toml.NewEncoder(&b).Encode(wf); err != nil {
			return c.Fail("workflow show", err)
		}
		fmt.Fprint(c.Stdout, b.String())
		return 0
	case "run":
		var out workflowRunView
		body := map[string]any{}
		if len(repos) > 0 {
			body["repositories"] = []string(repos)
		}
		if err := cl.Do(ctx, http.MethodPost, "/api/v1/workflows/"+name+"/run", body, &out); err != nil {
			return c.Fail("workflow run", err)
		}
		fmt.Fprintf(c.Stdout, "run %s created from %s@%d: %d task(s)\n", short(out.RunID), name, wf.Generation, len(out.Works))
		for _, w := range out.Works {
			fmt.Fprintf(c.Stdout, "  %s  %s\n", short(w.Work.ID), w.Work.Title)
		}
		return 0
	case "runs":
		var out []workflowRunRow
		if err := cl.Do(ctx, http.MethodGet, "/api/v1/workflows/"+name+"/runs", nil, &out); err != nil {
			return c.Fail("workflow runs", err)
		}
		if *asJSON {
			c.PrintJSON(out)
			return 0
		}
		tw := tabwriter.NewWriter(c.Stdout, 0, 4, 2, ' ', 0)
		fmt.Fprintln(tw, "RUN\tSTATE\tCREATED\tSTEPS")
		for _, run := range out {
			steps := make([]string, len(run.Works))
			for i, w := range run.Works {
				steps[i] = w.Work.WorkflowStep + ":" + string(w.State)
			}
			fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", short(run.RunID), run.State, run.CreatedAt, strings.Join(steps, " "))
		}
		if err := tw.Flush(); err != nil {
			return c.Fail("workflow runs", err)
		}
		return 0
	case "enable", "disable":
		wf.ScheduleEnabled = sub == "enable"
		if wf.ScheduleEnabled && wf.Schedule == "" {
			fmt.Fprintln(c.Stderr, "forge workflow enable: the workflow has no schedule")
			return 2
		}
		if err := cl.Do(ctx, http.MethodPut, fmt.Sprintf("/api/v1/workflows/%s?generation=%d", name, wf.Generation), wf, &wf); err != nil {
			return c.Fail("workflow "+sub, err)
		}
		fmt.Fprintf(c.Stdout, "workflow %s schedule %sd (generation %d)\n", name, sub, wf.Generation)
		return 0
	case "edit":
		edited, err := editWorkflow(ctx, c, wf, *from)
		if err != nil {
			return c.Fail("workflow edit", err)
		}
		if err := cl.Do(ctx, http.MethodPut, fmt.Sprintf("/api/v1/workflows/%s?generation=%d", name, wf.Generation), edited, &edited); err != nil {
			return c.Fail("workflow edit", err)
		}
		fmt.Fprintf(c.Stdout, "workflow %s updated (generation %d)\n", name, edited.Generation)
		return 0
	}
	return 2
}

// editWorkflow opens the workflow as TOML in $EDITOR (or reads --from) and
// returns the result; the generation the user saw goes back for the 409 check.
func editWorkflow(ctx context.Context, c *Context, wf store.Workflow, from string) (store.Workflow, error) {
	if from != "" {
		if _, err := toml.DecodeFile(from, &wf); err != nil {
			return wf, err
		}
		return wf, nil
	}
	edited := wf
	err := editTOML(ctx, c, "forge-workflow-*.toml", wf, &edited)
	if err != nil {
		return wf, err
	}
	edited.Name = wf.Name
	return edited, nil
}
