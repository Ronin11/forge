package main

import (
	"context"
	"fmt"
	"net/http"
	"net/url"
	"strings"
	"text/tabwriter"
	"time"

	"forge/internal/store"
)

func runProposal(ctx context.Context, c *cmdContext, args []string) int {
	if len(args) == 0 || strings.HasPrefix(args[0], "-") {
		fmt.Fprintln(c.stderr, "usage: forge proposal list|show|approve|reject [flags]")
		return 2
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
	fmt.Fprintf(c.stderr, "forge proposal: unknown subcommand %q\n", args[0])
	return 2
}

func runProposalList(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("proposal list")
	status := fs.String("status", "", "only this status: proposed|approved|rejected|applied|reverted")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.resolveLogging(lf, "cli.proposal")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("proposal list", err)
	}
	path := "/api/v1/proposals"
	if *status != "" {
		path += "?status=" + url.QueryEscape(*status)
	}
	var out []store.Proposal
	if err := cl.do(ctx, http.MethodGet, path, nil, &out); err != nil {
		return c.fail("proposal list", err)
	}
	if *asJSON {
		c.printJSON(out)
		return 0
	}
	tw := tabwriter.NewWriter(c.stdout, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "ID\tKIND\tSTATUS\tTARGET\tAGE\tRATIONALE")
	now := time.Now()
	for _, p := range out {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%s\n", short(p.ID), p.Kind, p.Status, p.Target, ago(p.CreatedAt, now), firstRunes(p.Rationale, 60))
	}
	if err := tw.Flush(); err != nil {
		return c.fail("proposal list", err)
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

func runProposalShow(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("proposal show")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.stderr, "usage: forge proposal show ID")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.proposal")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("proposal show", err)
	}
	var p store.Proposal
	if err := cl.do(ctx, http.MethodGet, "/api/v1/proposals/"+url.PathEscape(fs.Arg(0)), nil, &p); err != nil {
		return c.fail("proposal show", err)
	}
	if *asJSON {
		c.printJSON(p)
		return 0
	}
	fmt.Fprintf(c.stdout, "proposal %s  %s  kind %s  source %s\n", p.ID, p.Status, p.Kind, p.Source)
	fmt.Fprintf(c.stdout, "target: %s\n", p.Target)
	fmt.Fprintf(c.stdout, "created: %s\n", p.CreatedAt.Format(time.RFC3339))
	if p.DecidedBy != "" {
		fmt.Fprintf(c.stdout, "decided: %s by %s\n", p.DecidedAt.Format(time.RFC3339), p.DecidedBy)
	}
	fmt.Fprintf(c.stdout, "rationale: %s\n", p.Rationale)
	fmt.Fprintf(c.stdout, "verification plan: %s\n", p.VerificationPlan)
	if len(p.Before) > 0 {
		fmt.Fprintf(c.stdout, "before: %s\n", p.Before)
	}
	if len(p.After) > 0 {
		fmt.Fprintf(c.stdout, "after: %s\n", p.After)
	}
	if p.AppliedRef != "" {
		fmt.Fprintf(c.stdout, "applied ref: %s\n", p.AppliedRef)
	}
	if len(p.OutcomeMetrics) > 0 {
		fmt.Fprintf(c.stdout, "outcome metrics: %s\n", p.OutcomeMetrics)
	}
	return 0
}

func runProposalApprove(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("proposal approve")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() != 1 {
		fmt.Fprintln(c.stderr, "usage: forge proposal approve ID")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.proposal")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("proposal approve", err)
	}
	var p store.Proposal
	if err := cl.do(ctx, http.MethodPost, "/api/v1/proposals/"+url.PathEscape(fs.Arg(0))+"/approve", nil, &p); err != nil {
		return c.fail("proposal approve", err)
	}
	if p.AppliedRef != "" {
		fmt.Fprintf(c.stdout, "proposal %s approved and applied (%s)\n", short(p.ID), p.AppliedRef)
	} else {
		fmt.Fprintf(c.stdout, "proposal %s approved (%s)\n", short(p.ID), p.Status)
	}
	return 0
}

func runProposalReject(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("proposal reject")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() < 1 {
		fmt.Fprintln(c.stderr, "usage: forge proposal reject ID [\"reason\"]")
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.proposal")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("proposal reject", err)
	}
	var body any
	if reason := strings.Join(fs.Args()[1:], " "); reason != "" {
		body = map[string]string{"reason": reason}
	}
	var out struct {
		Proposal store.Proposal `json:"proposal"`
		Reason   string         `json:"reason"`
	}
	if err := cl.do(ctx, http.MethodPost, "/api/v1/proposals/"+url.PathEscape(fs.Arg(0))+"/reject", body, &out); err != nil {
		return c.fail("proposal reject", err)
	}
	fmt.Fprintf(c.stdout, "proposal %s rejected\n", short(out.Proposal.ID))
	return 0
}
