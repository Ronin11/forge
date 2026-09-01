package tui

import (
	"context"
	"fmt"
	"net/http"
	"strings"
	"text/tabwriter"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// queueRow is one entry of GET /api/v1/queue.
type queueRow struct {
	Work     store.Work      `json:"work"`
	State    model.WorkState `json:"state"`
	Reason   string          `json:"reason"`
	Waiting  []string        `json:"waiting"`
	Position int             `json:"position"`
	Targets  []store.Target  `json:"targets"`
}

func RunQueue(ctx context.Context, c *Context, args []string) int {
	sub := "list"
	if len(args) > 0 && !strings.HasPrefix(args[0], "-") {
		sub, args = args[0], args[1:]
	}
	switch sub {
	case "list":
	case "move":
		return runQueueMove(ctx, c, args)
	case "block":
		return runQueueBlock(ctx, c, args)
	default:
		fmt.Fprintf(c.Stderr, "forge queue: unknown subcommand %q\n", sub)
		return 2
	}
	fs, lf := c.Flags("queue list")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.ResolveLogging(lf, "cli.queue")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("queue", err)
	}
	var rows []queueRow
	if err := cl.Do(ctx, http.MethodGet, "/api/v1/queue", nil, &rows); err != nil {
		return c.Fail("queue", err)
	}
	if *asJSON {
		c.PrintJSON(rows)
		return 0
	}
	tw := tabwriter.NewWriter(c.Stdout, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "#\tID\tSTATE\tPRIO\tCLASS\tROUTINE\tREASON")
	for _, r := range rows {
		reason := r.Reason
		if len(r.Waiting) > 0 {
			ids := make([]string, len(r.Waiting))
			for i, w := range r.Waiting {
				ids[i] = short(w)
			}
			reason += " (" + strings.Join(ids, ",") + ")"
		}
		fmt.Fprintf(tw, "%d\t%s\t%s\t%d\t%s\t%s\t%s\n", r.Position, short(r.Work.ID), r.State, r.Work.Priority, r.Work.BudgetClass, r.Work.RoutineName, reason)
	}
	if err := tw.Flush(); err != nil {
		return c.Fail("queue", err)
	}
	return 0
}

// runQueueMove reorders one task before another; the daemon owns the rule (a
// task never moves above one it is blocked by) so the CLI and the UI agree.
func runQueueMove(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("queue move")
	before := fs.String("before", "", "the task this one should run before")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 || *before == "" {
		fmt.Fprintln(c.Stderr, "usage: forge queue move ID --before ID")
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.queue")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("queue move", err)
	}
	if err := cl.Do(ctx, http.MethodPatch, "/api/v1/work/"+fs.Arg(0), map[string]string{"move_before": *before}, nil); err != nil {
		return c.Fail("queue move", err)
	}
	fmt.Fprintf(c.Stdout, "moved %s before %s\n", short(fs.Arg(0)), short(*before))
	return 0
}

// runQueueBlock adds or removes a blocked_by edge.
func runQueueBlock(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("queue block")
	on := fs.String("on", "", "the task this one waits for")
	remove := fs.Bool("remove", false, "remove the edge instead")
	onKind := fs.String("when", "success", "success|terminal: when the dependency is satisfied")
	stack := fs.Bool("stack", false, "stack: start on the dependency's branch head before it merges (M9)")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 || *on == "" {
		fmt.Fprintln(c.Stderr, "usage: forge queue block ID --on ID [--remove] [--when success|terminal]")
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.queue")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("queue block", err)
	}
	var body map[string]any
	if *remove {
		body = map[string]any{"remove_blocked_by": []string{*on}}
	} else {
		body = map[string]any{"add_blocked_by": []map[string]any{{"work_id": *on, "on": *onKind, "stack_on": *stack}}}
	}
	if err := cl.Do(ctx, http.MethodPatch, "/api/v1/work/"+fs.Arg(0), body, nil); err != nil {
		return c.Fail("queue block", err)
	}
	verb := "blocked"
	if *remove {
		verb = "unblocked"
	}
	fmt.Fprintf(c.Stdout, "%s %s on %s\n", verb, short(fs.Arg(0)), short(*on))
	return 0
}
