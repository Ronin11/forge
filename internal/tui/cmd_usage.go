package tui

import (
	"context"
	"fmt"
	"forge/internal/core/engine"
	"net/http"
	"time"
)

// usageView mirrors GET /api/v1/usage (handlers_usage.go's usageResponse).
type usageView struct {
	SchemaVersion int          `json:"schema_version"`
	Usage         engine.Usage `json:"usage"`
	Config        struct {
		FiveHourTarget   float64 `json:"five_hour_target"`
		SevenDayTarget   float64 `json:"seven_day_target"`
		FiveHourHardStop float64 `json:"five_hour_hard_stop"`
		SevenDayHardStop float64 `json:"seven_day_hard_stop"`
		DailyUSDCap      float64 `json:"daily_usd_cap"`
	} `json:"config"`
}

// RunUsage prints the budget windows of DESIGN.md §10.1: utilization, rates,
// target rate, delta, and forecast per window, plus calibration and daily cost.
func RunUsage(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("usage")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() > 0 {
		fmt.Fprintf(c.Stderr, "forge usage: unexpected argument %q\n", fs.Arg(0))
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.usage")
	if code >= 0 {
		return code
	}
	cl := c.Client(log)
	if err := cl.Connect(ctx); err != nil {
		return c.Fail("usage", err)
	}
	var v usageView
	if err := cl.Do(ctx, http.MethodGet, "/api/v1/usage", nil, &v); err != nil {
		return c.Fail("usage", err)
	}
	if *asJSON {
		c.PrintJSON(v)
		return 0
	}
	now := c.Now()
	printUsageWindow(c, v.Usage.FiveHour, v.Config.FiveHourTarget, now)
	printUsageWindow(c, v.Usage.SevenDay, v.Config.SevenDayTarget, now)
	u := v.Usage
	if u.TokensPerPoint > 0 {
		fmt.Fprintf(c.Stdout, "tokens/point       %.0f (median over recent attempts)\n", u.TokensPerPoint)
	} else {
		fmt.Fprintln(c.Stdout, "tokens/point       no calibration data yet")
	}
	queued := fmt.Sprintf("queued estimate    +%.1f%%", u.QueuedEstimate*100)
	if u.QueuedUnknown > 0 {
		queued += fmt.Sprintf(" (%d task(s) without an estimate)", u.QueuedUnknown)
	}
	fmt.Fprintln(c.Stdout, queued)
	if v.Config.DailyUSDCap > 0 {
		fmt.Fprintf(c.Stdout, "daily spend        $%.2f of $%.2f cap\n", u.DailyUSD, v.Config.DailyUSDCap)
	} else {
		fmt.Fprintf(c.Stdout, "daily spend        $%.2f (no cap)\n", u.DailyUSD)
	}
	return 0
}

// printUsageWindow renders one window block; a window with no samples yet says
// so instead of printing zeros that look like data.
func printUsageWindow(c *Context, w engine.WindowUsage, target float64, now time.Time) {
	fmt.Fprintln(c.Stdout, w.Window)
	if w.Utilization < 0 {
		fmt.Fprintln(c.Stdout, "  no samples yet")
		return
	}
	fmt.Fprintf(c.Stdout, "  utilization      %.1f%%  resets in %s\n", w.Utilization*100, untilText(w.ResetsAt, now))
	fmt.Fprintf(c.Stdout, "  rates 1h/6h/24h  %.1f%%/h  %.1f%%/h  %.1f%%/h\n", w.Rate1h*100, w.Rate6h*100, w.Rate24h*100)
	trend := "burning faster than target"
	if w.Delta <= 0 {
		trend = "burning slower than target"
	}
	fmt.Fprintf(c.Stdout, "  target           %.1f%%  target rate %.1f%%/h  delta %+.1f%%/h (%s)\n", target*100, w.TargetRate*100, w.Delta*100, trend)
	fmt.Fprintf(c.Stdout, "  forecast at reset  %.1f%% (target %.1f%%)\n", w.ForecastAtReset*100, target*100)
	if w.LastResetUnspent != nil {
		fmt.Fprintf(c.Stdout, "  last reset unspent %.1f%%\n", *w.LastResetUnspent*100)
	}
}

// untilText renders time-to-reset for the table.
func untilText(t time.Time, now time.Time) string {
	if t.IsZero() {
		return "unknown"
	}
	d := t.Sub(now).Round(time.Minute)
	if d <= 0 {
		return "passed"
	}
	if d < time.Hour {
		return fmt.Sprintf("%dm", int(d.Minutes()))
	}
	return fmt.Sprintf("%dh%02dm", int(d.Hours()), int(d.Minutes())%60)
}
