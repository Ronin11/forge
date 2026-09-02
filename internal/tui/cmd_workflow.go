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
		fmt.Fprintln(c.Stderr, "usage: forge workflow add|list|show|edit|run|runs|run-show|retry|cancel|enable|disable NAME [flags]")
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
	case "run-show", "cancel", "retry":
		return runWorkflowRunOp(ctx, c, sub, rest)
	}
	fmt.Fprintf(c.Stderr, "forge workflow: unknown subcommand %q\n", sub)
	return 2
}

// workflowRunDetailView is GET /api/v1/workflow-runs/{id}.
type workflowRunDetailView struct {
	ID           string            `json:"id"`
	WorkflowName string            `json:"workflow_name"`
	Status       string            `json:"status"`
	Trigger      string            `json:"trigger"`
	ScriptRuns   int               `json:"script_runs"`
	CreatedAt    string            `json:"created_at"`
	FinishedAt   string            `json:"finished_at,omitempty"`
	Nodes        []workflowRunNode `json:"nodes"`
}

// resolveWorkflowRunID accepts a full run id or a unique prefix over every
// workflow's recent runs.
func resolveWorkflowRunID(ctx context.Context, cl *cliClient, prefix string) (string, error) {
	if len(prefix) == 32 {
		return prefix, nil
	}
	var wfs []store.Workflow
	if err := cl.Do(ctx, http.MethodGet, "/api/v1/workflows?archived=true", nil, &wfs); err != nil {
		return "", err
	}
	var matches []string
	for _, wf := range wfs {
		var runs []workflowRunRow
		if err := cl.Do(ctx, http.MethodGet, "/api/v1/workflows/"+wf.Name+"/runs", nil, &runs); err != nil {
			return "", err
		}
		for _, run := range runs {
			if strings.HasPrefix(run.RunID, prefix) {
				matches = append(matches, run.RunID)
			}
		}
	}
	switch len(matches) {
	case 1:
		return matches[0], nil
	case 0:
		return "", fmt.Errorf("no workflow run matches %q", prefix)
	}
	return "", fmt.Errorf("%q matches %d runs; be more specific", prefix, len(matches))
}

// runWorkflowRunOp is the run-addressed half: run-show, cancel, and retry.
func runWorkflowRunOp(ctx context.Context, c *Context, sub string, args []string) int {
	fs, lf := c.Flags("workflow " + sub)
	asJSON := fs.Bool("json", false, "JSON output")
	fromNode := fs.String("from", "", "node to retry from (retry only)")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintf(c.Stderr, "usage: forge workflow %s RUN_ID\n", sub)
		return 2
	}
	if sub == "retry" && *fromNode == "" {
		fmt.Fprintln(c.Stderr, "usage: forge workflow retry RUN_ID --from NODE")
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.workflow")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("workflow "+sub, err)
	}
	id, err := resolveWorkflowRunID(ctx, cl, fs.Arg(0))
	if err != nil {
		return c.Fail("workflow "+sub, err)
	}
	switch sub {
	case "cancel":
		var out map[string]string
		if err := cl.Do(ctx, http.MethodPost, "/api/v1/workflow-runs/"+id+"/cancel", map[string]any{}, &out); err != nil {
			return c.Fail("workflow cancel", err)
		}
		fmt.Fprintf(c.Stdout, "run %s cancelling\n", short(id))
		return 0
	case "retry":
		var out map[string]string
		if err := cl.Do(ctx, http.MethodPost, "/api/v1/workflow-runs/"+id+"/retry", map[string]string{"node": *fromNode}, &out); err != nil {
			return c.Fail("workflow retry", err)
		}
		fmt.Fprintf(c.Stdout, "run %s retrying from %s\n", short(id), *fromNode)
		return 0
	}
	var out workflowRunDetailView
	if err := cl.Do(ctx, http.MethodGet, "/api/v1/workflow-runs/"+id, nil, &out); err != nil {
		return c.Fail("workflow run-show", err)
	}
	if *asJSON {
		c.PrintJSON(out)
		return 0
	}
	fmt.Fprintf(c.Stdout, "run %s  workflow %s  %s  trigger %s  scripts %d\n", short(out.ID), out.WorkflowName, out.Status, out.Trigger, out.ScriptRuns)
	tw := tabwriter.NewWriter(c.Stdout, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "NODE\tITER\tTYPE\tSTATUS\tWORK\tERROR")
	for _, n := range out.Nodes {
		fmt.Fprintf(tw, "%s\t%d\t%s\t%s\t%s\t%s\n", n.NodeID, n.Iteration, n.Type, n.Status, short(n.WorkID), n.Error)
	}
	if err := tw.Flush(); err != nil {
		return c.Fail("workflow run-show", err)
	}
	return 0
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
	nodes := 0
	if out.Graph != nil {
		nodes = len(out.Graph.Nodes)
	}
	fmt.Fprintf(c.Stdout, "workflow %s created with %d node(s) (generation %d)\n", out.Name, nodes, out.Generation)
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
		var names []string
		if wf.Graph != nil {
			names = make([]string, len(wf.Graph.Nodes))
			for i, n := range wf.Graph.Nodes {
				names[i] = n.ID
			}
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

// workflowRunView is POST /api/v1/workflows/{name}/run's 201 body: the run's
// Works do not exist yet — the engine materializes nodes as they become ready.
type workflowRunView struct {
	RunID      string `json:"run_id"`
	Workflow   string `json:"workflow"`
	Generation int    `json:"generation"`
}

// workflowRunNode is one node instance in a runs listing or run detail.
type workflowRunNode struct {
	NodeID    string `json:"node_id"`
	Iteration int    `json:"iteration"`
	Type      string `json:"type"`
	Status    string `json:"status"`
	WorkID    string `json:"work_id,omitempty"`
	Error     string `json:"error,omitempty"`
}

// workflowRunRow is one row of GET /api/v1/workflows/{name}/runs: an engine
// run carries Nodes, a pre-engine run carries Works and legacy = true.
type workflowRunRow struct {
	RunID     string            `json:"run_id"`
	State     string            `json:"state"`
	Trigger   string            `json:"trigger,omitempty"`
	CreatedAt string            `json:"created_at"`
	Nodes     []workflowRunNode `json:"nodes,omitempty"`
	Works     []taskView        `json:"works,omitempty"`
	Legacy    bool              `json:"legacy,omitempty"`
}

// runSteps renders a run's per-node (or, for legacy runs, per-work) states.
func runSteps(run workflowRunRow) string {
	if run.Legacy {
		steps := make([]string, len(run.Works))
		for i, w := range run.Works {
			steps[i] = w.Work.WorkflowStep + ":" + string(w.State)
		}
		return strings.Join(steps, " ")
	}
	steps := make([]string, len(run.Nodes))
	for i, n := range run.Nodes {
		label := n.NodeID
		if n.Iteration > 1 {
			label = fmt.Sprintf("%s#%d", n.NodeID, n.Iteration)
		}
		steps[i] = label + ":" + n.Status
	}
	return strings.Join(steps, " ")
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
		fmt.Fprintf(c.Stdout, "run %s created from %s@%d\n", short(out.RunID), name, out.Generation)
		fmt.Fprintf(c.Stdout, "  forge workflow run-show %s\n", short(out.RunID))
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
			fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", short(run.RunID), run.State, run.CreatedAt, runSteps(run))
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
