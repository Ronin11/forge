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

// queueRow is one entry of GET /api/v1/queue.
type queueRow struct {
	Work     store.Work      `json:"work"`
	State    model.WorkState `json:"state"`
	Reason   string          `json:"reason"`
	Waiting  []string        `json:"waiting"`
	Position int             `json:"position"`
	Targets  []store.Target  `json:"targets"`
}

func runQueue(ctx context.Context, c *cmdContext, args []string) int {
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
		fmt.Fprintf(c.stderr, "forge queue: unknown subcommand %q\n", sub)
		return 2
	}
	fs, lf := c.flags("queue list")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.resolveLogging(lf, "cli.queue")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("queue", err)
	}
	var rows []queueRow
	if err := cl.do(ctx, http.MethodGet, "/api/v1/queue", nil, &rows); err != nil {
		return c.fail("queue", err)
	}
	if *asJSON {
		c.printJSON(rows)
		return 0
	}
	tw := tabwriter.NewWriter(c.stdout, 0, 4, 2, ' ', 0)
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
		return c.fail("queue", err)
	}
	return 0
}

// runQueueMove reorders one task before another; the daemon owns the rule (a
// task never moves above one it is blocked by) so the CLI and the UI agree.
func runQueueMove(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("queue move")
	before := fs.String("before", "", "the task this one should run before")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 || *before == "" {
		fmt.Fprintln(c.stderr, "usage: forge queue move ID --before ID")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.queue")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("queue move", err)
	}
	if err := cl.do(ctx, http.MethodPatch, "/api/v1/work/"+fs.Arg(0), map[string]string{"move_before": *before}, nil); err != nil {
		return c.fail("queue move", err)
	}
	fmt.Fprintf(c.stdout, "moved %s before %s\n", short(fs.Arg(0)), short(*before))
	return 0
}

// runQueueBlock adds or removes a blocked_by edge.
func runQueueBlock(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("queue block")
	on := fs.String("on", "", "the task this one waits for")
	remove := fs.Bool("remove", false, "remove the edge instead")
	onKind := fs.String("when", "success", "success|terminal: when the dependency is satisfied")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 || *on == "" {
		fmt.Fprintln(c.stderr, "usage: forge queue block ID --on ID [--remove] [--when success|terminal]")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.queue")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("queue block", err)
	}
	var body map[string]any
	if *remove {
		body = map[string]any{"remove_blocked_by": []string{*on}}
	} else {
		body = map[string]any{"add_blocked_by": []map[string]string{{"work_id": *on, "on": *onKind}}}
	}
	if err := cl.do(ctx, http.MethodPatch, "/api/v1/work/"+fs.Arg(0), body, nil); err != nil {
		return c.fail("queue block", err)
	}
	verb := "blocked"
	if *remove {
		verb = "unblocked"
	}
	fmt.Fprintf(c.stdout, "%s %s on %s\n", verb, short(fs.Arg(0)), short(*on))
	return 0
}
