// Budget is the M3 admission engine: the usage model of DESIGN.md §10.1 and the
// admission policy of §10.2. ComputeUsage and Decide are pure (STYLE.md §2: the
// budget admission decision has exactly one home); BudgetPolicy adapts them to
// the SchedulerPolicy seam from live store reads with a short cache.
package engine

import (
	"context"
	"sort"
	"sync"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/model"
	"forge/internal/core/store"
)

// Window lengths L_w of DESIGN.md §10.1.
const (
	fiveHourLen = 5 * time.Hour
	sevenDayLen = 168 * time.Hour
)

// usageCacheTTL is how long BudgetPolicy.Decide reuses a loaded Usage so claim
// traffic does not hammer SQLite.
const usageCacheTTL = 10 * time.Second

// sampleLoadWindow is how far back the policy reads samples: the 24 h rate
// horizon plus one hour of slack so the pair that straddles the horizon is
// present. A gap longer than this contributes no delta.
const sampleLoadWindow = 25 * time.Hour

// factsLoadWindow is how far back the policy reads facts for calibration and
// queued estimates; it always covers local midnight for DailyUSD.
const factsLoadWindow = 7 * 24 * time.Hour

// WindowUsage is everything the policy and the usage report know about one
// subscription window (DESIGN.md §10.1).
type WindowUsage struct {
	// Window is "five_hour" or "seven_day".
	Window string `json:"window"`
	// Utilization u_w from the latest sample; -1 when no sample exists yet.
	Utilization float64 `json:"utilization"`
	// ResetsAt and SampledAt come from the same latest sample.
	ResetsAt  time.Time `json:"resets_at,omitzero"`
	SampledAt time.Time `json:"sampled_at,omitzero"`
	// FractionElapsed is f = (now − (resets_at − L_w)) / L_w, clamped to [0, 1];
	// 0 when there is no sample.
	FractionElapsed float64 `json:"fraction_elapsed"`
	// Rate1h/6h/24h are r_w(h): sum of positive utilization deltas between
	// consecutive samples in the last h hours, divided by h. A reset boundary
	// (utilization drop OR resets_at change) is not a negative delta and its
	// pair is skipped.
	Rate1h  float64 `json:"rate_1h"`
	Rate6h  float64 `json:"rate_6h"`
	Rate24h float64 `json:"rate_24h"`
	// TargetRate is max(0, target_w − u_w) / hours_to_reset — the burn rate that
	// lands exactly on the target at reset; 0 when unknown or past the reset.
	TargetRate float64 `json:"target_rate"`
	// Delta is Rate1h − TargetRate (positive = burning faster than needed).
	Delta float64 `json:"delta"`
	// ForecastAtReset is u_w + Rate1h·hours_to_reset + Σ_queued(expected_delta).
	ForecastAtReset float64 `json:"forecast_at_reset"`
	// LastResetUnspent is target_w − u just before the most recent resets_at
	// change in the sample window (§10.2's unspent_at_reset); nil if none.
	LastResetUnspent *float64 `json:"last_reset_unspent,omitempty"`
}

// Usage is the full §10.1 usage model, what `forge usage` and the dashboard show
// and what Decide evaluates.
type Usage struct {
	FiveHour WindowUsage `json:"five_hour"`
	SevenDay WindowUsage `json:"seven_day"`
	// TokensPerPoint is the calibration median over recent facts of
	// (input + output + cache_creation) / utilization_delta_estimate where the
	// delta is non-NULL and > 0; 0 when there is no data yet.
	TokensPerPoint float64 `json:"tokens_per_point"`
	// QueuedEstimate is Σ over queued Work of its routine's median
	// utilization_delta_estimate (NULL/absent → 0, flagged in QueuedUnknown).
	QueuedEstimate float64 `json:"queued_estimate"`
	// QueuedUnknown counts queued Work whose routine has no estimate.
	QueuedUnknown int `json:"queued_unknown"`
	// DailyUSD is Σ cost_usd of facts finished since local midnight (local as
	// carried by now's location; the timestamps are wall clock, DESIGN §9.2).
	DailyUSD float64 `json:"daily_usd"`
}

// ComputeUsage is the pure §10.1 model: it sees only its inputs. samples maps
// window name to that window's samples, oldest first (SamplesSince order);
// facts feed calibration and DailyUSD; queuedRoutines carries one routine name
// per queued Work; factsByRoutine feeds the per-routine median deltas.
func ComputeUsage(now time.Time, cfg config.BudgetConfig, samples map[string][]store.RateLimitSample, facts []store.AttemptFacts, queuedRoutines []string, factsByRoutine map[string][]store.AttemptFacts) Usage {
	u := Usage{
		TokensPerPoint: tokensPerPoint(facts),
		DailyUSD:       dailyUSD(now, facts),
	}
	u.QueuedEstimate, u.QueuedUnknown = queuedEstimate(queuedRoutines, factsByRoutine)
	u.FiveHour = windowUsage(now, "five_hour", fiveHourLen, cfg.FiveHourTarget, samples["five_hour"], u.QueuedEstimate)
	u.SevenDay = windowUsage(now, "seven_day", sevenDayLen, cfg.SevenDayTarget, samples["seven_day"], u.QueuedEstimate)
	return u
}

// windowUsage evaluates every §10.1 formula for one window; queued is the
// Σ_queued(expected_delta) term of the forecast.
func windowUsage(now time.Time, name string, length time.Duration, target float64, samples []store.RateLimitSample, queued float64) WindowUsage {
	w := WindowUsage{Window: name, Utilization: -1}
	if len(samples) == 0 {
		return w
	}
	last := samples[len(samples)-1]
	w.Utilization, w.ResetsAt, w.SampledAt = last.Utilization, last.ResetsAt, last.Time
	if !last.ResetsAt.IsZero() {
		// f = (now − window_start) / L_w with window_start = resets_at − L_w,
		// clamped to [0, 1]. Wall-clock arithmetic by necessity (§10.1).
		f := now.Sub(last.ResetsAt.Add(-length)).Hours() / length.Hours()
		w.FractionElapsed = min(max(f, 0), 1)
	}
	w.Rate1h = rate(now, samples, 1)
	w.Rate6h = rate(now, samples, 6)
	w.Rate24h = rate(now, samples, 24)
	hrs := max(last.ResetsAt.Sub(now).Hours(), 0)
	if hrs > 0 {
		// target_rate_w = max(0, target_w − u_w) / hours_to_reset.
		w.TargetRate = max(target-last.Utilization, 0) / hrs
	}
	w.Delta = w.Rate1h - w.TargetRate
	w.ForecastAtReset = last.Utilization + w.Rate1h*hrs + queued
	w.LastResetUnspent = lastResetUnspent(samples, target)
	return w
}

// rate is r_w(h): the sum of positive utilization deltas between consecutive
// samples whose later sample falls in (now − h, now], divided by h. A reset
// boundary pair is skipped, never counted as a negative delta.
func rate(now time.Time, samples []store.RateLimitSample, hours float64) float64 {
	cutoff := now.Add(-time.Duration(hours * float64(time.Hour)))
	var sum float64
	for i := 1; i < len(samples); i++ {
		if !samples[i].Time.After(cutoff) || isResetBoundary(samples[i-1], samples[i]) {
			continue
		}
		if d := samples[i].Utilization - samples[i-1].Utilization; d > 0 {
			sum += d
		}
	}
	return sum / hours
}

// isResetBoundary is §10.1's boundary rule: a drop in utilization or a change
// in resets_at between consecutive samples marks a window reset.
func isResetBoundary(prev, cur store.RateLimitSample) bool {
	return cur.Utilization < prev.Utilization || !cur.ResetsAt.Equal(prev.ResetsAt)
}

// lastResetUnspent finds the most recent resets_at change among the samples and
// returns target − the utilization just before it (§10.2's unspent_at_reset).
func lastResetUnspent(samples []store.RateLimitSample, target float64) *float64 {
	for i := len(samples) - 1; i >= 1; i-- {
		if !samples[i].ResetsAt.Equal(samples[i-1].ResetsAt) {
			v := target - samples[i-1].Utilization
			return &v
		}
	}
	return nil
}

// tokensPerPoint is §10.1's calibration: the median over facts of
// (input + output + cache_creation) / utilization_delta_estimate where the
// delta is non-NULL and > 0; 0 with no usable fact.
func tokensPerPoint(facts []store.AttemptFacts) float64 {
	var ratios []float64
	for _, f := range facts {
		if f.UtilizationDelta == nil || *f.UtilizationDelta <= 0 {
			continue
		}
		var tokens int64
		for _, p := range []*int64{f.InputTokens, f.OutputTokens, f.CacheCreation} {
			if p != nil {
				tokens += *p
			}
		}
		ratios = append(ratios, float64(tokens) / *f.UtilizationDelta)
	}
	return median(ratios)
}

// queuedEstimate is §10.1's Σ_queued(expected_delta): each queued Work
// contributes its routine's median utilization_delta_estimate; a Work whose
// routine has no non-NULL estimate contributes 0 and is flagged.
func queuedEstimate(queuedRoutines []string, factsByRoutine map[string][]store.AttemptFacts) (estimate float64, unknown int) {
	medians := map[string]float64{}
	for routine, facts := range factsByRoutine {
		var deltas []float64
		for _, f := range facts {
			if f.UtilizationDelta != nil {
				deltas = append(deltas, *f.UtilizationDelta)
			}
		}
		if len(deltas) > 0 {
			medians[routine] = median(deltas)
		}
	}
	for _, routine := range queuedRoutines {
		m, ok := medians[routine]
		if !ok {
			unknown++
			continue
		}
		estimate += m
	}
	return estimate, unknown
}

// dailyUSD sums cost_usd of facts finished since midnight in now's location —
// the input of §10.2's daily_usd_cap gate.
func dailyUSD(now time.Time, facts []store.AttemptFacts) float64 {
	midnight := time.Date(now.Year(), now.Month(), now.Day(), 0, 0, 0, 0, now.Location())
	var sum float64
	for _, f := range facts {
		if f.CostUSD != nil && !f.FinishedAt.Before(midnight) {
			sum += *f.CostUSD
		}
	}
	return sum
}

// median is the middle value of xs (the mean of the middle two for an even
// count); 0 for no values. xs is sorted in place.
func median(xs []float64) float64 {
	if len(xs) == 0 {
		return 0
	}
	sort.Float64s(xs)
	mid := len(xs) / 2
	if len(xs)%2 == 1 {
		return xs[mid]
	}
	return (xs[mid-1] + xs[mid]) / 2
}

// Decide is THE admission rule, §10.2's ordered list, pure and table-tested.
// Both windows must admit; a window with no samples yet admits.
func Decide(now time.Time, u Usage, class model.BudgetClass, cfg config.BudgetConfig) (admit bool, reason string) {
	windows := []struct {
		w        WindowUsage
		target   float64
		hardStop float64
	}{
		{u.FiveHour, cfg.FiveHourTarget, cfg.FiveHourHardStop},
		{u.SevenDay, cfg.SevenDayTarget, cfg.SevenDayHardStop},
	}
	// 1. u ≥ hard_stop defers every class; so does the daily USD cap when set.
	for _, e := range windows {
		if e.w.Utilization >= 0 && e.w.Utilization >= e.hardStop {
			return false, "hard_stop:" + e.w.Window
		}
	}
	if cfg.DailyUSDCap > 0 && u.DailyUSD >= cfg.DailyUSDCap {
		return false, "daily_usd_cap"
	}
	// 2. interactive → admit.
	if class == model.ClassInteractive {
		return true, ""
	}
	// 3. Quiet hours: non-interactive classes admit only while u < target −
	// reserve for both windows.
	if InQuietHours(now, cfg.QuietHours) {
		for _, e := range windows {
			if e.w.Utilization >= 0 && e.w.Utilization >= e.target-cfg.QuietHours.Reserve {
				return false, "quiet_hours"
			}
		}
	}
	// 4. normal's rule: u < target and the 1 h-rate forecast at reset ≤ target.
	for _, e := range windows {
		if e.w.Utilization < 0 {
			continue
		}
		if e.w.Utilization >= e.target {
			return false, "over_target:" + e.w.Window
		}
		hrs := max(e.w.ResetsAt.Sub(now).Hours(), 0)
		if cfg.ForecastPacing && e.w.Utilization+e.w.Rate1h*hrs > e.target {
			return false, "forecast_over_target:" + e.w.Window
		}
	}
	if class != model.ClassBacklog {
		return true, ""
	}
	// 5. backlog: additionally u < target · f — the burn-down line, low early in
	// a window and rising to the target as the reset nears.
	for _, e := range windows {
		if e.w.Utilization < 0 {
			continue
		}
		if e.w.Utilization >= e.target*e.w.FractionElapsed {
			return false, "ahead_of_burn_down_line:" + e.w.Window
		}
	}
	return true, ""
}

// InQuietHours reports whether now's clock time (in its own location) falls in
// the configured local-time window; [start, end) may wrap midnight. An
// unconfigured section never matches; the HH:MM format is validated at config
// load, so a parse failure here just disables the gate.
func InQuietHours(now time.Time, q config.QuietHoursConfig) bool {
	if q.Start == "" || q.End == "" {
		return false
	}
	start, err := time.Parse("15:04", q.Start)
	if err != nil {
		return false
	}
	end, err := time.Parse("15:04", q.End)
	if err != nil {
		return false
	}
	minutes := func(t time.Time) int { return t.Hour()*60 + t.Minute() }
	n, s, e := minutes(now), minutes(start), minutes(end)
	if s <= e {
		return n >= s && n < e
	}
	return n >= s || n < e
}

// BudgetPolicy implements SchedulerPolicy from live store reads. Decide caches
// the loaded Usage for usageCacheTTL so claim traffic does not hammer SQLite;
// Usage always loads fresh for the API and CLI.
type BudgetPolicy struct {
	store *store.Store
	cfg   config.BudgetConfig
	clock func() time.Time

	// mu guards cached and cachedAt (the Decide-side usage cache).
	mu       sync.Mutex
	cached   Usage
	cachedAt time.Time
}

// NewBudgetPolicy builds the M3 policy; a nil clock means time.Now.
func NewBudgetPolicy(st *store.Store, cfg config.BudgetConfig, clock func() time.Time) *BudgetPolicy {
	if clock == nil {
		clock = time.Now
	}
	return &BudgetPolicy{store: st, cfg: cfg, clock: clock}
}

// Config is the policy's validated [budget] section, exposed for the usage
// report (the usageReporter capability).
func (p *BudgetPolicy) Config() config.BudgetConfig { return p.cfg }

// Decide loads (or reuses) Usage and applies the pure rule. The SchedulerPolicy
// seam carries no context, so the load runs on Background; when the store
// cannot be read the class is deferred rather than admitted unmetered.
func (p *BudgetPolicy) Decide(class model.BudgetClass) (bool, string) {
	u, err := p.cachedUsage(context.Background())
	if err != nil {
		return false, "usage_unavailable"
	}
	return Decide(p.clock(), u, class, p.cfg)
}

// Usage is the uncached load for GET /api/v1/usage and forge usage.
func (p *BudgetPolicy) Usage(ctx context.Context) (Usage, error) {
	return p.load(ctx, p.clock())
}

// cachedUsage returns the cached Usage while it is younger than usageCacheTTL,
// loading and caching otherwise. The freshness check subtracts clock readings —
// wall arithmetic, acceptable for a cache TTL and required for injected clocks.
func (p *BudgetPolicy) cachedUsage(ctx context.Context) (Usage, error) {
	now := p.clock()
	p.mu.Lock()
	defer p.mu.Unlock()
	if !p.cachedAt.IsZero() && now.Sub(p.cachedAt) < usageCacheTTL {
		return p.cached, nil
	}
	u, err := p.load(ctx, now)
	if err != nil {
		return Usage{}, err
	}
	p.cached, p.cachedAt = u, now
	return u, nil
}

// load gathers ComputeUsage's inputs from the store: recent samples per window,
// recent facts (calibration, medians, daily cost), and the queued Work.
func (p *BudgetPolicy) load(ctx context.Context, now time.Time) (Usage, error) {
	samples := map[string][]store.RateLimitSample{}
	for _, w := range []string{"five_hour", "seven_day"} {
		ss, err := p.store.SamplesSince(ctx, w, now.Add(-sampleLoadWindow))
		if err != nil {
			return Usage{}, err
		}
		samples[w] = ss
	}
	facts, err := p.store.FactsSince(ctx, now.Add(-factsLoadWindow), now.Add(time.Second), "")
	if err != nil {
		return Usage{}, err
	}
	byRoutine := map[string][]store.AttemptFacts{}
	for _, f := range facts {
		byRoutine[f.Routine] = append(byRoutine[f.Routine], f)
	}
	queued, err := p.queuedRoutines(ctx)
	if err != nil {
		return Usage{}, err
	}
	return ComputeUsage(now, p.cfg, samples, facts, queued, byRoutine), nil
}

// queuedRoutines returns one routine name per queued Work: open Work with at
// least one pending Target. Dependency and budget eligibility are not
// re-evaluated here — the estimate is a forecast input, and recursing into the
// policy to size its own queue would bite its tail.
func (p *BudgetPolicy) queuedRoutines(ctx context.Context) ([]string, error) {
	open, err := p.store.OpenWork(ctx)
	if err != nil {
		return nil, err
	}
	ids := make([]string, len(open))
	for i, w := range open {
		ids[i] = w.ID
	}
	targets, err := p.store.TargetsForWorks(ctx, ids)
	if err != nil {
		return nil, err
	}
	var queued []string
	for _, w := range open {
		for _, t := range targets[w.ID] {
			if t.State == model.Pending {
				queued = append(queued, w.RoutineName)
				break
			}
		}
	}
	return queued, nil
}

// BudgetReset is the payload of a "budget.reset" journal row: the durable
// unspent_at_reset metric of §10.2, written at sample ingestion.
type BudgetReset struct {
	Window       string    `json:"window"`
	Unspent      float64   `json:"unspent"`
	PrevResetsAt time.Time `json:"prev_resets_at"`
	NewResetsAt  time.Time `json:"new_resets_at"`
}

// BudgetResets detects resets_at changes per window between the latest stored
// sample (prev; a nil or absent entry journals nothing for the first incoming
// sample) and the incoming batch, including changes within the batch. unspent
// is the window's target minus the last utilization before the reset.
func BudgetResets(cfg config.BudgetConfig, prev map[string]*store.RateLimitSample, incoming []store.RateLimitSample) []BudgetReset {
	targets := map[string]float64{"five_hour": cfg.FiveHourTarget, "seven_day": cfg.SevenDayTarget}
	last := map[string]*store.RateLimitSample{}
	for w, s := range prev {
		last[w] = s
	}
	var out []BudgetReset
	for i := range incoming {
		s := incoming[i]
		if p := last[s.Window]; p != nil && !p.ResetsAt.Equal(s.ResetsAt) {
			out = append(out, BudgetReset{Window: s.Window, Unspent: targets[s.Window] - p.Utilization, PrevResetsAt: p.ResetsAt, NewResetsAt: s.ResetsAt})
		}
		last[s.Window] = &incoming[i]
	}
	return out
}
