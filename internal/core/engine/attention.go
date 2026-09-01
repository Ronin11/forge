package engine

import (
	"time"

	"forge/internal/core/config"
	"forge/internal/core/model"
	"forge/internal/core/store"
)

// AttentionDeadline is when q auto-decides, and whether it does at all. critical
// (and auto-decision off) never does. The wait is time-of-day aware: shorter in
// quiet hours, when no one is watching; low criticality burns down at the quiet
// wait even during active hours. Shared by the sweep and the Human Queue UI so
// the countdown the operator sees is the deadline the sweep acts on.
func AttentionDeadline(q store.Question, now time.Time, cfg config.AttentionConfig, quiet config.QuietHoursConfig) (time.Time, bool) {
	crit := model.Criticality(q.Criticality)
	if crit == "" {
		crit = model.CriticalityNormal
	}
	if !crit.AutoDecidable() || !cfg.AutoDecideOn() {
		return time.Time{}, false
	}
	wait := cfg.WaitActiveMinutes
	if InQuietHours(now, quiet) {
		wait = cfg.WaitQuietMinutes
	}
	if crit == model.CriticalityLow && cfg.WaitQuietMinutes < wait {
		wait = cfg.WaitQuietMinutes
	}
	if wait < 1 {
		return time.Time{}, false
	}
	return q.AskedAt.Add(time.Duration(wait) * time.Minute), true
}
