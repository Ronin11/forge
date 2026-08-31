package main

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"text/tabwriter"
	"time"

	"forge/internal/model"
	"forge/internal/store"
)

// multiFlag collects a repeated flag.
type multiFlag []string

func (m *multiFlag) String() string     { return strings.Join(*m, ",") }
func (m *multiFlag) Set(v string) error { *m = append(*m, v); return nil }

// taskView is what GET /api/v1/tasks/{id} returns.
type taskView struct {
	Work      store.Work       `json:"work"`
	State     model.WorkState  `json:"state"`
	Targets   []store.Target   `json:"targets"`
	Attempts  []store.Attempt  `json:"attempts"`
	Questions []store.Question `json:"questions"`
}

func runTask(ctx context.Context, c *cmdContext, args []string) int {
	if len(args) == 0 || strings.HasPrefix(args[0], "-") {
		fmt.Fprintln(c.stderr, "usage: forge task add|list|show|logs|cancel|answer|approve|reject [flags]")
		return 2
	}
	switch args[0] {
	case "add":
		return runTaskAdd(ctx, c, args[1:])
	case "list":
		return runTaskList(ctx, c, args[1:])
	case "show":
		return runTaskShow(ctx, c, args[1:])
	case "logs":
		return runTaskLogs(ctx, c, args[1:])
	case "cancel":
		return runTaskCancel(ctx, c, args[1:])
	case "answer":
		return runTaskAnswer(ctx, c, args[1:])
	case "approve":
		return runTaskApprove(ctx, c, args[1:])
	case "reject":
		return runTaskReject(ctx, c, args[1:])
	}
	fmt.Fprintf(c.stderr, "forge task: unknown subcommand %q\n", args[0])
	return 2
}

func runTaskAdd(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("task add")
	var repos, after, paths multiFlag
	fs.Var(&repos, "repo", "repository name or path (repeatable)")
	fs.Var(&after, "after", "task id this task waits for (repeatable)")
	fs.Var(&paths, "paths", "write-set glob (repeatable)")
	mode := fs.String("mode", "", "mode (default run)")
	routine := fs.String("routine", "", "routine to run (the prompt, if given, overrides its prompt)")
	priority := fs.Int("priority", 0, "queue priority (default 100 for human submissions)")
	class := fs.String("class", "", "budget class: interactive|normal|backlog")
	autonomy := fs.String("autonomy", "", "ask|checkpoint|notify|auto")
	modelAlias := fs.String("model", "", "model alias (default haiku)")
	integrate := fs.Bool("integrate", false, "queue for merge after success (M9)")
	title := fs.String("title", "", "short title (default: the prompt's first line)")
	wait := fs.Bool("wait", false, "wait for the task to finish and exit with its outcome")
	asJSON := fs.Bool("json", false, "print the created task as JSON")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	prompt := strings.TrimSpace(strings.Join(fs.Args(), " "))
	if prompt == "" && *routine == "" {
		fmt.Fprintln(c.stderr, "forge task add: a prompt is required (or --routine)")
		return 2
	}
	if len(repos) == 0 && *routine == "" {
		fmt.Fprintln(c.stderr, "forge task add: --repo is required without --routine")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.task")
	if code >= 0 {
		return code
	}
	// A path-form --repo (DESIGN §1.3) resolves to an absolute path here: the
	// daemon's cwd is not the user's.
	for i, r := range repos {
		if strings.ContainsRune(r, os.PathSeparator) {
			abs, err := filepath.Abs(r)
			if err != nil {
				return c.fail("task add", err)
			}
			repos[i] = abs
		}
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("task add", err)
	}
	body := map[string]any{"prompt": prompt, "repositories": []string(repos), "mode": *mode, "routine": *routine, "priority": *priority, "class": *class, "autonomy": *autonomy, "model": *modelAlias, "after": []string(after), "paths": []string(paths), "integrate": *integrate, "title": *title}
	var out taskView
	// On a fresh home the daemon was auto-started moments ago and the worker
	// registers repositories a beat later; "task add on a freshly initialised
	// machine must work with no other setup" (DESIGN §1), so an unregistered
	// repository is retried briefly instead of failed (M6 smoke 7).
	deadline := time.Now().Add(20 * time.Second)
	waited := false
	for {
		err := cl.do(ctx, http.MethodPost, "/api/v1/tasks", body, &out)
		if err == nil {
			break
		}
		if !strings.Contains(err.Error(), "is not registered") || time.Now().After(deadline) {
			return c.fail("task add", err)
		}
		if !waited {
			fmt.Fprintln(c.stderr, "waiting for the worker to register repositories…")
			waited = true
		}
		select {
		case <-ctx.Done():
			return c.fail("task add", ctx.Err())
		case <-time.After(time.Second):
		}
	}
	if *asJSON {
		c.printJSON(out)
	} else {
		state := string(out.State)
		if state == "" {
			state = "pending"
		}
		fmt.Fprintf(c.stdout, "task %s created (%s, %d target(s))\n", short(out.Work.ID), state, len(out.Targets))
	}
	if !*wait {
		return 0
	}
	return waitForTask(ctx, c, cl, out.Work.ID)
}

// waitForTask follows the task's SSE stream until it reaches a state that
// maps to an exit code. Terminal states arrive as the stream's end event;
// waiting_human and conflict pause the Work without ending it, so every
// journal row triggers one state read. The stream reconnects across a daemon
// drain-restart (DESIGN.md §1.4).
func waitForTask(ctx context.Context, c *cmdContext, cl *cliClient, id string) int {
	last := model.WorkState("")
	exit := -1
	report := func(state model.WorkState) bool {
		if state == "" {
			return false
		}
		if state != last {
			fmt.Fprintf(c.stderr, "task %s: %s\n", short(id), state)
			last = state
		}
		switch state {
		case model.WorkSucceeded, model.WorkMerged:
			exit = 0
		case model.WorkFailed, model.WorkPartial, model.WorkCancelled:
			exit = 1
		case model.WorkUnverified:
			exit = 3
		case model.WorkWaitingHuman, model.WorkConflict:
			exit = 4
		default:
			return false
		}
		return true
	}
	hooks := streamHooks{onJournal: func(store.JournalEntry) bool {
		var v taskView
		if err := cl.do(ctx, http.MethodGet, "/api/v1/tasks/"+id, nil, &v); err != nil {
			return false // transient (a restart in flight); the stream carries on
		}
		return report(v.State)
	}}
	state, err := followWork(ctx, cl, id, true, hooks)
	if exit >= 0 {
		return exit
	}
	if err != nil {
		return c.fail("task add --wait", err)
	}
	if report(state) {
		return exit
	}
	return c.fail("task add --wait", fmt.Errorf("stream ended in state %q", state))
}

func runTaskList(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("task list")
	state := fs.String("state", "", "only tasks in this state")
	limit := fs.Int("limit", 50, "how many")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.resolveLogging(lf, "cli.task")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("task list", err)
	}
	var out []taskView
	if err := cl.do(ctx, http.MethodGet, fmt.Sprintf("/api/v1/tasks?limit=%d", *limit), nil, &out); err != nil {
		return c.fail("task list", err)
	}
	if *asJSON {
		c.printJSON(out)
		return 0
	}
	tw := tabwriter.NewWriter(c.stdout, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "ID\tSTATE\tROUTINE\tREPOS\tCLASS\tPRIO\tAGE\tTITLE")
	now := time.Now()
	for _, t := range out {
		if *state != "" && string(t.State) != *state {
			continue
		}
		repos := make([]string, 0, len(t.Targets))
		for _, tg := range t.Targets {
			repos = append(repos, tg.Repository)
		}
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%d\t%s\t%s\n", short(t.Work.ID), t.State, t.Work.RoutineName, strings.Join(repos, ","), t.Work.BudgetClass, t.Work.Priority, ago(t.Work.CreatedAt, now), t.Work.Title)
	}
	if err := tw.Flush(); err != nil {
		return c.fail("task list", err)
	}
	return 0
}

// resolveTaskID accepts a full id or a unique prefix over open+recent tasks.
func resolveTaskID(ctx context.Context, cl *cliClient, prefix string) (string, error) {
	if len(prefix) == 32 {
		return prefix, nil
	}
	var out []taskView
	if err := cl.do(ctx, http.MethodGet, "/api/v1/tasks?limit=200", nil, &out); err != nil {
		return "", err
	}
	var matches []string
	for _, t := range out {
		if strings.HasPrefix(t.Work.ID, prefix) {
			matches = append(matches, t.Work.ID)
		}
	}
	switch len(matches) {
	case 1:
		return matches[0], nil
	case 0:
		return "", fmt.Errorf("no task matches %q", prefix)
	}
	return "", fmt.Errorf("%q matches %d tasks; be more specific", prefix, len(matches))
}

func runTaskShow(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("task show")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.stderr, "usage: forge task show ID")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.task")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("task show", err)
	}
	id, err := resolveTaskID(ctx, cl, fs.Arg(0))
	if err != nil {
		return c.fail("task show", err)
	}
	var v taskView
	if err := cl.do(ctx, http.MethodGet, "/api/v1/tasks/"+id, nil, &v); err != nil {
		return c.fail("task show", err)
	}
	if *asJSON {
		c.printJSON(v)
		return 0
	}
	fmt.Fprintf(c.stdout, "task %s  %s  routine %s@%d  class %s  priority %d  autonomy %s\n", v.Work.ID, v.State, v.Work.RoutineName, v.Work.Generation, v.Work.BudgetClass, v.Work.Priority, v.Work.Autonomy)
	fmt.Fprintf(c.stdout, "title: %s\n", v.Work.Title)
	for _, t := range v.Targets {
		line := fmt.Sprintf("  target %s  %s  %s", short(t.ID), t.Repository, t.State)
		if t.FailureReason != "" {
			line += "  reason=" + string(t.FailureReason)
		}
		if t.UnverifiedReason != "" {
			line += "  unverified=" + t.UnverifiedReason
		}
		if t.Retained {
			line += "  retained"
		}
		fmt.Fprintln(c.stdout, line)
	}
	for _, a := range v.Attempts {
		line := fmt.Sprintf("  attempt %s  launches %d  turns %d  tokens in/out %d/%d", short(a.ID), a.Launches, a.NumTurns, a.Usage.InputTokens, a.Usage.OutputTokens)
		if a.CostUSD != nil {
			line += fmt.Sprintf("  cost $%.4f", *a.CostUSD)
		}
		if a.Cleanup.Outcome != "" {
			line += "  cleanup=" + a.Cleanup.Outcome + " (" + a.Cleanup.Reason + ")"
		}
		fmt.Fprintln(c.stdout, line)
		if a.Branch != "" {
			fmt.Fprintf(c.stdout, "    branch %s  base %s  head %s  commits %d  pushed %v\n", a.Branch, short(a.BaseCommit), short(a.HeadCommit), a.Git.Commits, a.Git.Pushed)
		}
		if a.ResultText != "" {
			fmt.Fprintf(c.stdout, "    result: %s\n", strings.TrimSpace(firstLines(a.ResultText, 6)))
		}
		if a.Cleanup.Command != "" {
			fmt.Fprintf(c.stdout, "    cleanup: %s\n", a.Cleanup.Command)
		}
	}
	for _, q := range v.Questions {
		state := "open"
		if !q.AnsweredAt.IsZero() {
			state = "answered: " + q.Answer
		}
		fmt.Fprintf(c.stdout, "  question %s  %s  (%s)\n", short(q.ID), q.Text, state)
	}
	return 0
}

func firstLines(s string, n int) string {
	lines := strings.Split(s, "\n")
	if len(lines) > n {
		lines = append(lines[:n], "…")
	}
	return strings.Join(lines, "\n    ")
}

func runTaskCancel(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("task cancel")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.stderr, "usage: forge task cancel ID")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.task")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("task cancel", err)
	}
	id, err := resolveTaskID(ctx, cl, fs.Arg(0))
	if err != nil {
		return c.fail("task cancel", err)
	}
	if err := cl.do(ctx, http.MethodDelete, "/api/v1/tasks/"+id, nil, nil); err != nil {
		return c.fail("task cancel", err)
	}
	fmt.Fprintf(c.stdout, "task %s cancel requested\n", short(id))
	return 0
}

func runTaskAnswer(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("task answer")
	question := fs.String("question", "", "question id when the task has several open")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() < 2 {
		fmt.Fprintln(c.stderr, "usage: forge task answer ID \"answer\"")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.task")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("task answer", err)
	}
	id, err := resolveTaskID(ctx, cl, fs.Arg(0))
	if err != nil {
		return c.fail("task answer", err)
	}
	var v taskView
	if err := cl.do(ctx, http.MethodGet, "/api/v1/tasks/"+id, nil, &v); err != nil {
		return c.fail("task answer", err)
	}
	var open []store.Question
	for _, q := range v.Questions {
		if q.AnsweredAt.IsZero() && (*question == "" || strings.HasPrefix(q.ID, *question)) {
			open = append(open, q)
		}
	}
	switch len(open) {
	case 0:
		fmt.Fprintln(c.stderr, "forge task answer: no open question")
		return 2
	case 1:
	default:
		fmt.Fprintf(c.stderr, "forge task answer: %d open questions; pick one with --question\n", len(open))
		return 2
	}
	answer := strings.Join(fs.Args()[1:], " ")
	if err := cl.do(ctx, http.MethodPost, "/api/v1/questions/"+open[0].ID+"/answer", map[string]string{"answer": answer, "by": "human"}, nil); err != nil {
		return c.fail("task answer", err)
	}
	fmt.Fprintf(c.stdout, "answered %s; task %s re-queued\n", short(open[0].ID), short(v.Work.ID))
	return 0
}

func runTaskApprove(ctx context.Context, c *cmdContext, args []string) int {
	return runTaskDecide(ctx, c, args, "approve")
}

func runTaskReject(ctx context.Context, c *cmdContext, args []string) int {
	return runTaskDecide(ctx, c, args, "reject")
}

// runTaskDecide is L3 (VERIFICATION.md): find the task's verifying Target and
// post the human's decision; approve → succeeded, reject → unverified with
// reason human_rejected.
func runTaskDecide(ctx context.Context, c *cmdContext, args []string, action string) int {
	fs, lf := c.flags("task " + action)
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if (action == "approve" && fs.NArg() != 1) || (action == "reject" && fs.NArg() < 2) {
		if action == "approve" {
			fmt.Fprintln(c.stderr, "usage: forge task approve ID")
		} else {
			fmt.Fprintln(c.stderr, "usage: forge task reject ID \"reason\"")
		}
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.task")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("task "+action, err)
	}
	id, err := resolveTaskID(ctx, cl, fs.Arg(0))
	if err != nil {
		return c.fail("task "+action, err)
	}
	var v taskView
	if err := cl.do(ctx, http.MethodGet, "/api/v1/tasks/"+id, nil, &v); err != nil {
		return c.fail("task "+action, err)
	}
	var target *store.Target
	states := make([]string, 0, len(v.Targets))
	for i, t := range v.Targets {
		states = append(states, t.Repository+"="+string(t.State))
		if t.State == model.Verifying && target == nil {
			target = &v.Targets[i]
		}
	}
	if target == nil {
		fmt.Fprintf(c.stderr, "forge task %s: no target of task %s is verifying (%s)\n", action, short(id), strings.Join(states, ", "))
		return 2
	}
	body := map[string]string{"by": "human"}
	if action == "reject" {
		body["reason"] = strings.Join(fs.Args()[1:], " ")
	}
	var out store.Target
	if err := cl.do(ctx, http.MethodPost, "/api/v1/targets/"+target.ID+"/"+action, body, &out); err != nil {
		return c.fail("task "+action, err)
	}
	verb := "approved"
	if action == "reject" {
		verb = "rejected"
	}
	line := fmt.Sprintf("target %s (%s) %s → %s", short(out.ID), out.Repository, verb, out.State)
	if out.UnverifiedReason != "" {
		line += " (" + out.UnverifiedReason + ")"
	}
	fmt.Fprintln(c.stdout, line)
	return 0
}
