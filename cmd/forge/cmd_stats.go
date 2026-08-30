package main

import (
	"context"
	"fmt"
	"net/http"
	"net/url"
	"text/tabwriter"
	"time"

	"forge/internal/stats"
)

// statsEnvelope mirrors GET /api/v1/stats's body.
type statsEnvelope struct {
	SchemaVersion int           `json:"schema_version"`
	Report        *stats.Report `json:"report"`
}

// runStats prints the DESIGN.md §9.4 report: one table per routine with its
// generation rows indented, and the previous-window deltas where known.
func runStats(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("stats")
	since := fs.String("since", "7d", "window: <N>h hours or <N>d days")
	routine := fs.String("routine", "", "limit to one routine")
	repository := fs.String("repository", "", "limit to one repository")
	project := fs.String("project", "", "limit to one project")
	mode := fs.String("mode", "", "limit to one mode")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() > 0 {
		fmt.Fprintf(c.stderr, "forge stats: unexpected argument %q\n", fs.Arg(0))
		return 2
	}
	_, log, code := c.resolveLogging(lf, "cli.stats")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	if err := cl.connect(ctx); err != nil {
		return c.fail("stats", err)
	}
	v := url.Values{}
	v.Set("since", *since)
	for key, val := range map[string]string{"routine": *routine, "repository": *repository, "project": *project, "mode": *mode} {
		if val != "" {
			v.Set(key, val)
		}
	}
	var out statsEnvelope
	if err := cl.do(ctx, http.MethodGet, "/api/v1/stats?"+v.Encode(), nil, &out); err != nil {
		return c.fail("stats", err)
	}
	if *asJSON {
		c.printJSON(out)
		return 0
	}
	if err := printStats(c, out.Report); err != nil {
		return c.fail("stats", err)
	}
	return 0
}

// printStats renders the report. Each routine gets a header line and a table:
// the all-generations rollup row first, then one indented row per generation.
func printStats(c *cmdContext, r *stats.Report) error {
	if r == nil || len(r.Routines) == 0 {
		fmt.Fprintln(c.stdout, "no attempts in the window")
		return nil
	}
	fmt.Fprintf(c.stdout, "window %s → %s  (%d runs)\n", r.Query.Since.Format(time.RFC3339), r.Query.Until.Format(time.RFC3339), r.TotalRuns)
	byRoutine := map[string][]stats.RoutineStats{}
	for _, g := range r.Generations {
		byRoutine[g.Routine] = append(byRoutine[g.Routine], g)
	}
	for _, rt := range r.Routines {
		fmt.Fprintf(c.stdout, "\n%s\n", rt.Routine)
		tw := tabwriter.NewWriter(c.stdout, 0, 4, 2, ' ', 0)
		fmt.Fprintln(tw, "  GEN\tRUNS\tVERIFIED\tSELF\tP50\tP95\tTOK IN/OUT\tCOST/RUN\tTOP FAILURE\tPREV")
		statsRow(tw, "all", rt, r.Prev)
		for _, g := range byRoutine[rt.Routine] {
			statsRow(tw, fmt.Sprintf("  @%d", g.Generation), g, r.Prev)
		}
		if err := tw.Flush(); err != nil {
			return fmt.Errorf("render table: %w", err)
		}
	}
	return nil
}

// statsRow writes one table row; label is "all" or the indented "@N".
func statsRow(tw *tabwriter.Writer, label string, s stats.RoutineStats, prev map[string]stats.RoutineStats) {
	fmt.Fprintf(tw, "  %s\t%d\t%s\t%s\t%s\t%s\t%.0f/%.0f\t$%.4f\t%s\t%s\n",
		label, s.Runs, pct(s.VerifiedSuccessRate), pct(s.SelfReportedSuccessRate),
		fmtUS(s.P50TotalUS), fmtUS(s.P95TotalUS), s.TokensInPerRun, s.TokensOutPerRun,
		s.CostPerRun, topFailure(s), prevDelta(s, prev))
}

// prevDelta renders the change against the previous equal window: the
// verified-rate delta in percentage points and the cost-per-run delta.
func prevDelta(cur stats.RoutineStats, prev map[string]stats.RoutineStats) string {
	p, ok := prev[stats.Key(cur.Routine, cur.Generation)]
	if !ok {
		return "-"
	}
	return fmt.Sprintf("%+.0f%% / $%+.4f", (cur.VerifiedSuccessRate-p.VerifiedSuccessRate)*100, cur.CostPerRun-p.CostPerRun)
}

func pct(v float64) string { return fmt.Sprintf("%.0f%%", v*100) }

func topFailure(s stats.RoutineStats) string {
	if len(s.TopFailureReasons) == 0 {
		return "-"
	}
	r := s.TopFailureReasons[0]
	return fmt.Sprintf("%s(%d)", r.Reason, r.Count)
}

// fmtUS renders a microsecond duration for tables; 0 means no measured runs.
func fmtUS(us int64) string {
	if us == 0 {
		return "-"
	}
	d := time.Duration(us) * time.Microsecond
	switch {
	case d >= time.Minute:
		return d.Round(time.Second).String()
	case d >= time.Second:
		return d.Round(100 * time.Millisecond).String()
	case d >= time.Millisecond:
		return d.Round(time.Millisecond).String()
	}
	return d.String()
}
