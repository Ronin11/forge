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
	case "move", "block":
		fmt.Fprintf(c.stderr, "forge queue %s: arrives in M3\n", sub)
		return 2
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
