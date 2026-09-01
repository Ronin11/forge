package tui

import (
	"context"
	"fmt"
	"net/http"
	"net/url"
	"strings"
	"text/tabwriter"
	"time"

	"forge/internal/core/store"
)

func RunProposal(ctx context.Context, c *Context, args []string) int {
	if len(args) == 0 || strings.HasPrefix(args[0], "-") {
		// --help on a parent command is a request, not a mistake.
		code := 2
		if len(args) > 0 && (args[0] == "--help" || args[0] == "-h") {
			code = 0
		}
		fmt.Fprintln(c.Stderr, "usage: forge proposal list|show|approve|reject [flags]")
		return code
	}
	switch args[0] {
	case "list":
		return runProposalList(ctx, c, args[1:])
	case "show":
		return runProposalShow(ctx, c, args[1:])
	case "approve":
		return runProposalApprove(ctx, c, args[1:])
	case "reject":
		return runProposalReject(ctx, c, args[1:])
	}
	fmt.Fprintf(c.Stderr, "forge proposal: unknown subcommand %q\n", args[0])
	return 2
}

func runProposalList(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("proposal list")
	status := fs.String("status", "", "only this status: proposed|approved|rejected|applied|reverted")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.ResolveLogging(lf, "cli.proposal")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("proposal list", err)
	}
	path := "/api/v1/proposals"
	if *status != "" {
		path += "?status=" + url.QueryEscape(*status)
	}
	var out []store.Proposal
	if err := cl.Do(ctx, http.MethodGet, path, nil, &out); err != nil {
		return c.Fail("proposal list", err)
	}
	if *asJSON {
		c.PrintJSON(out)
		return 0
	}
	tw := tabwriter.NewWriter(c.Stdout, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "ID\tKIND\tSTATUS\tTARGET\tAGE\tRATIONALE")
	now := time.Now()
	for _, p := range out {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%s\n", short(p.ID), p.Kind, p.Status, p.Target, ago(p.CreatedAt, now), firstRunes(p.Rationale, 60))
	}
	if err := tw.Flush(); err != nil {
		return c.Fail("proposal list", err)
	}
	return 0
}

// firstRunes is s's first line, bounded to n runes for tables.
func firstRunes(s string, n int) string {
	line, _, _ := strings.Cut(strings.TrimSpace(s), "\n")
	if r := []rune(line); len(r) > n {
		return string(r[:n]) + "…"
	}
	return line
}

func runProposalShow(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("proposal show")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.Stderr, "usage: forge proposal show ID")
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.proposal")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("proposal show", err)
	}
	var p store.Proposal
	if err := cl.Do(ctx, http.MethodGet, "/api/v1/proposals/"+url.PathEscape(fs.Arg(0)), nil, &p); err != nil {
		return c.Fail("proposal show", err)
	}
	if *asJSON {
		c.PrintJSON(p)
		return 0
	}
	fmt.Fprintf(c.Stdout, "proposal %s  %s  kind %s  source %s\n", p.ID, p.Status, p.Kind, p.Source)
	fmt.Fprintf(c.Stdout, "target: %s\n", p.Target)
	fmt.Fprintf(c.Stdout, "created: %s\n", p.CreatedAt.Format(time.RFC3339))
	if p.DecidedBy != "" {
		fmt.Fprintf(c.Stdout, "decided: %s by %s\n", p.DecidedAt.Format(time.RFC3339), p.DecidedBy)
	}
	fmt.Fprintf(c.Stdout, "rationale: %s\n", p.Rationale)
	fmt.Fprintf(c.Stdout, "verification plan: %s\n", p.VerificationPlan)
	if len(p.Before) > 0 {
		fmt.Fprintf(c.Stdout, "before: %s\n", p.Before)
	}
	if len(p.After) > 0 {
		fmt.Fprintf(c.Stdout, "after: %s\n", p.After)
	}
	if p.AppliedRef != "" {
		fmt.Fprintf(c.Stdout, "applied ref: %s\n", p.AppliedRef)
	}
	if len(p.OutcomeMetrics) > 0 {
		fmt.Fprintf(c.Stdout, "outcome metrics: %s\n", p.OutcomeMetrics)
	}
	return 0
}

func runProposalApprove(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("proposal approve")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.Stderr, "usage: forge proposal approve ID")
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.proposal")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("proposal approve", err)
	}
	var p store.Proposal
	if err := cl.Do(ctx, http.MethodPost, "/api/v1/proposals/"+url.PathEscape(fs.Arg(0))+"/approve", nil, &p); err != nil {
		return c.Fail("proposal approve", err)
	}
	if p.AppliedRef != "" {
		fmt.Fprintf(c.Stdout, "proposal %s approved and applied (%s)\n", short(p.ID), p.AppliedRef)
	} else {
		fmt.Fprintf(c.Stdout, "proposal %s approved (%s)\n", short(p.ID), p.Status)
	}
	return 0
}

func runProposalReject(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("proposal reject")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() < 1 {
		fmt.Fprintln(c.Stderr, "usage: forge proposal reject ID [\"reason\"]")
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.proposal")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("proposal reject", err)
	}
	var body any
	if reason := strings.Join(fs.Args()[1:], " "); reason != "" {
		body = map[string]string{"reason": reason}
	}
	var out struct {
		Proposal store.Proposal `json:"proposal"`
		Reason   string         `json:"reason"`
	}
	if err := cl.Do(ctx, http.MethodPost, "/api/v1/proposals/"+url.PathEscape(fs.Arg(0))+"/reject", body, &out); err != nil {
		return c.Fail("proposal reject", err)
	}
	fmt.Fprintf(c.Stdout, "proposal %s rejected\n", short(out.Proposal.ID))
	return 0
}
