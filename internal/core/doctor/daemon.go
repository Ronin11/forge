package doctor

import (
	"fmt"
	"sort"
	"strings"
	"time"

	"forge/internal/core/plugin"
	"forge/internal/core/store"
)

// kbStaleAfter is how old the kb index may be before it is a warn: the daemon
// reindexes every five minutes, so three missed ticks means the loop is stuck.
const kbStaleAfter = 15 * time.Minute

// budgetStaleAfter is how old the newest rate-limit sample may be before the
// budget policy is flying blind; samples arrive with every real attempt.
const budgetStaleAfter = 6 * time.Hour

// DaemonInput is everything the daemon-side checks decide over; the handler
// gathers it from the store and daemon.json, keeping SQL and file formats in
// their homes.
type DaemonInput struct {
	Version         string
	SchemaVersion   string
	StartedAt       time.Time // zero when daemon.json is unavailable
	Now             time.Time
	Workers         []store.Worker
	Repositories    []store.Repository
	KbLastIndexedAt time.Time // zero when nothing is indexed
	RetainedCount   int       // rows in retained_worktrees
	FiveHourSample  *store.RateLimitSample
	SevenDaySample  *store.RateLimitSample
	// AheadOfOrigin lists repositories whose local default branch carries
	// commits origin does not have. Agents base worktrees on the ORIGIN ref,
	// so unpushed local commits are invisible to every agent — the stale-base
	// overnight incident.
	AheadOfOrigin []RepoAhead
	Plugins       []store.Plugin        // installed plugin rows
	PluginHealth  []plugin.PluginHealth // the supervisor's live state
	// PricingPairs are recent attempts' notional cost (tokens × Forge's price
	// table) beside the executor's self-reported cost (M10, DESIGN.md §21);
	// large drift means the price table is stale.
	PricingPairs []PricingPair
}

// PricingPair is one attempt's notional-vs-reported cost, both in dollars.
type PricingPair struct {
	Notional float64
	Reported float64
}

// Daemon runs every daemon-side check over one gathered input.
func Daemon(in DaemonInput) []Check {
	checks := []Check{daemonInfo(in), schema(in)}
	checks = append(checks, workers(in)...)
	checks = append(checks, repositories(in), kbIndex(in), worktrees(in), budget(in), pricing(in))
	checks = append(checks, plugins(in)...)
	checks = append(checks, aheadOfOriginCheck(in.AheadOfOrigin)...)
	return checks
}

// pricingDriftThreshold is how far Forge's notional cost may sit from the
// executor's reported cost before the price table is flagged stale.
const pricingDriftThreshold = 0.15

// pricingMinSamples is how many priced attempts the drift check needs before it
// trusts the comparison.
const pricingMinSamples = 3

// pricing compares Forge's notional per-attempt cost (tokens × its price table)
// with the executor's self-reported total_cost_usd over recent attempts and
// warns when the mean relative drift exceeds the threshold — the signal that
// the embedded price table has gone stale (DESIGN.md §21).
func pricing(in DaemonInput) Check {
	var sumRel float64
	n := 0
	for _, p := range in.PricingPairs {
		if p.Reported <= 0 || p.Notional <= 0 {
			continue
		}
		sumRel += abs(p.Notional-p.Reported) / p.Reported
		n++
	}
	if n < pricingMinSamples {
		return Check{Name: "pricing", Status: StatusOK, Detail: fmt.Sprintf("not enough priced attempts to check drift (%d/%d)", n, pricingMinSamples)}
	}
	drift := sumRel / float64(n)
	detail := fmt.Sprintf("notional cost within %.0f%% of reported over %d attempts", drift*100, n)
	if drift > pricingDriftThreshold {
		return Check{Name: "pricing", Status: StatusWarn, Detail: fmt.Sprintf("notional cost drifts %.0f%% from reported over %d attempts", drift*100, n),
			Hint: "the [models] price table looks stale; refresh input/output/cache prices against the provider's list prices"}
	}
	return Check{Name: "pricing", Status: StatusOK, Detail: detail}
}

func abs(f float64) float64 {
	if f < 0 {
		return -f
	}
	return f
}

// plugins reports each enabled plugin: ok while its process runs, fail when
// it is down (the supervisor is backing off, or restart=never gave up).
// Disabled plugins are configuration, not health, and are skipped.
func plugins(in DaemonInput) []Check {
	byName := map[string]plugin.PluginHealth{}
	for _, h := range in.PluginHealth {
		byName[h.Name] = h
	}
	var out []Check
	for _, p := range in.Plugins {
		if !p.Enabled {
			continue
		}
		h := byName[p.Name]
		if h.Running {
			out = append(out, Check{Name: "plugin." + p.Name, Status: StatusOK,
				Detail: fmt.Sprintf("running (pid %d), %d restarts", h.PID, h.Restarts)})
			continue
		}
		detail := "enabled but not running"
		if h.LastExit != "" {
			detail += ", last exit: " + h.LastExit
		}
		if h.Restarts > 0 {
			detail += fmt.Sprintf(" (%d restarts)", h.Restarts)
		}
		out = append(out, Check{Name: "plugin." + p.Name, Status: StatusFail, Detail: detail,
			Hint: "forge plugin logs " + p.Name + " — restarts back off up to 60 s; a daemon restart or re-enable starts fresh"})
	}
	return out
}

func daemonInfo(in DaemonInput) Check {
	detail := "version " + in.Version
	if !in.StartedAt.IsZero() {
		detail += ", up " + in.Now.Sub(in.StartedAt).Round(time.Second).String()
	}
	return Check{Name: "daemon", Status: StatusOK, Detail: detail}
}

func schema(in DaemonInput) Check {
	if in.SchemaVersion == "" {
		return Check{Name: "schema", Status: StatusWarn, Detail: "no schema version recorded"}
	}
	return Check{Name: "schema", Status: StatusOK, Detail: in.SchemaVersion}
}

// workers reports registration, heartbeat age, and the capabilities each
// worker advertises (executors, browser); one row per concern so a red row
// names exactly what is wrong.
func workers(in DaemonInput) []Check {
	if len(in.Workers) == 0 {
		return []Check{{Name: "worker", Status: StatusWarn, Detail: "no worker has registered", Hint: "forge worker start (or forge service install)"}}
	}
	var out []Check
	for _, w := range in.Workers {
		age := in.Now.Sub(w.LastSeenAt).Round(time.Second)
		if w.Connected {
			out = append(out, Check{Name: "worker." + w.Name, Status: StatusOK, Detail: fmt.Sprintf("registered, heartbeat %s ago, %d/%d slots", age, w.Active, w.MaxConcurrent)})
		} else {
			out = append(out, Check{Name: "worker." + w.Name, Status: StatusWarn, Detail: fmt.Sprintf("last seen %s ago", age), Hint: "is the worker running? forge worker start"})
		}
		out = append(out, capabilityChecks(w)...)
	}
	return out
}

// capabilityChecks renders the worker's advertised capability map: executors
// (executor:<name> → ready|missing) and the browser.
func capabilityChecks(w store.Worker) []Check {
	names := make([]string, 0, len(w.Capabilities))
	for name := range w.Capabilities {
		names = append(names, name)
	}
	sort.Strings(names)
	var out []Check
	for _, name := range names {
		value := w.Capabilities[name]
		switch {
		case strings.HasPrefix(name, "executor:"):
			c := Check{Name: w.Name + "." + name, Status: StatusOK, Detail: value}
			if value != "ready" {
				c.Status = StatusWarn
				c.Hint = "install " + strings.TrimPrefix(name, "executor:") + "'s command on the worker's PATH"
			}
			out = append(out, c)
		case name == "browser":
			c := Check{Name: w.Name + ".browser", Status: StatusOK, Detail: value}
			if value != "ready" {
				c.Status = StatusWarn
				c.Hint = "forge init --with-browser installs Playwright under <home>/deps"
			}
			out = append(out, c)
		case name == "sandbox":
			// require_sandbox routines (default true) are not routed to a
			// sandbox:missing worker, so a missing sandbox is a failure, not
			// a nice-to-have (M8 smoke 6).
			c := Check{Name: w.Name + ".sandbox", Status: StatusOK, Detail: value}
			if value != "ready" {
				c.Status = StatusFail
				c.Hint = "install bubblewrap (bwrap) on the worker's PATH"
			}
			out = append(out, c)
		}
	}
	return out
}

func repositories(in DaemonInput) Check {
	if len(in.Repositories) == 0 {
		return Check{Name: "repositories", Status: StatusWarn, Detail: "none registered", Hint: "forge init, or add [repositories.<name>] to worker.toml"}
	}
	names := make([]string, len(in.Repositories))
	for i, r := range in.Repositories {
		names[i] = r.Name
	}
	return Check{Name: "repositories", Status: StatusOK, Detail: fmt.Sprintf("%d registered: %s", len(names), strings.Join(names, ", "))}
}

func kbIndex(in DaemonInput) Check {
	if in.KbLastIndexedAt.IsZero() {
		return Check{Name: "kb", Status: StatusWarn, Detail: "nothing indexed yet", Hint: "forge kb new writes the first note; the daemon indexes every 5 minutes"}
	}
	age := in.Now.Sub(in.KbLastIndexedAt).Round(time.Second)
	if age > kbStaleAfter {
		return Check{Name: "kb", Status: StatusWarn, Detail: fmt.Sprintf("index last updated %s ago", age), Hint: "POST /api/v1/kb/reindex, or check the daemon log's daemon.kb component"}
	}
	return Check{Name: "kb", Status: StatusOK, Detail: fmt.Sprintf("index updated %s ago", age)}
}

func worktrees(in DaemonInput) Check {
	if in.RetainedCount == 0 {
		return Check{Name: "worktrees", Status: StatusOK, Detail: "none retained"}
	}
	return Check{Name: "worktrees", Status: StatusWarn, Detail: fmt.Sprintf("%d retained", in.RetainedCount), Hint: "forge cleanup ATTEMPT_ID --confirm removes one after review"}
}

func budget(in DaemonInput) Check {
	newest := in.FiveHourSample
	if newest == nil || (in.SevenDaySample != nil && in.SevenDaySample.Time.After(newest.Time)) {
		newest = in.SevenDaySample
	}
	if newest == nil {
		return Check{Name: "budget", Status: StatusWarn, Detail: "no rate-limit samples yet", Hint: "samples arrive with the first real attempt; the policy admits conservatively until then"}
	}
	age := in.Now.Sub(newest.Time).Round(time.Second)
	detail := fmt.Sprintf("%s window at %.0f%%, sampled %s ago", newest.Window, newest.Utilization*100, age)
	if age > budgetStaleAfter {
		return Check{Name: "budget", Status: StatusWarn, Detail: detail, Hint: "no recent samples; the budget policy is deciding on stale data"}
	}
	return Check{Name: "budget", Status: StatusOK, Detail: detail}
}

// RepoAhead is one repository whose local branch outruns its origin.
type RepoAhead struct {
	Name   string `json:"name"`
	Branch string `json:"branch"`
	Ahead  int    `json:"ahead"`
}

// aheadOfOriginCheck warns per repository with unpushed local commits.
func aheadOfOriginCheck(ahead []RepoAhead) []Check {
	var out []Check
	for _, r := range ahead {
		if r.Ahead <= 0 {
			continue
		}
		out = append(out, Check{
			Name:   "repo_ahead_of_origin:" + r.Name,
			Status: "warn",
			Detail: fmt.Sprintf("%s: local %s is %d commit(s) ahead of origin — agents base on origin and cannot see them", r.Name, r.Branch, r.Ahead),
			Hint:   "push the branch (or drop the local commits); until then every agent works from stale code",
		})
	}
	return out
}
