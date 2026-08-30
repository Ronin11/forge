package controlplane

import (
	"context"
	"encoding/json"
	"math"
	"path/filepath"
	"testing"
	"time"

	"forge/internal/model"
	"forge/internal/protocol"
	"forge/internal/store"
)

var budgetCfg = BudgetConfig{FiveHourTarget: 0.9, SevenDayTarget: 0.9, FiveHourHardStop: 0.97, SevenDayHardStop: 0.97}

func bctx() context.Context { return context.Background() }

func near(a, b float64) bool { return math.Abs(a-b) < 1e-9 }

func fp(v float64) *float64 { return &v }

func sample(t time.Time, window string, u float64, resets time.Time) store.RateLimitSample {
	return store.RateLimitSample{Time: t, Window: window, Utilization: u, ResetsAt: resets}
}

func TestComputeUsageRates(t *testing.T) {
	now := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	resetsA := now.Add(-25 * time.Minute)
	resetsB := now.Add(4 * time.Hour)
	samples := map[string][]store.RateLimitSample{"five_hour": {
		sample(now.Add(-50*time.Minute), "five_hour", 0.6, resetsA),
		sample(now.Add(-40*time.Minute), "five_hour", 0.7, resetsA),
		sample(now.Add(-30*time.Minute), "five_hour", 0.8, resetsA),
		sample(now.Add(-20*time.Minute), "five_hour", 0.1, resetsB), // reset boundary
		sample(now.Add(-10*time.Minute), "five_hour", 0.25, resetsB),
	}}
	u := ComputeUsage(now, budgetCfg, samples, nil, nil, nil)
	w := u.FiveHour
	if w.Utilization != 0.25 || !w.ResetsAt.Equal(resetsB) || !w.SampledAt.Equal(now.Add(-10*time.Minute)) {
		t.Errorf("latest sample: %+v", w)
	}
	// Positive deltas on both sides of the boundary count; the boundary is skipped.
	if !near(w.Rate1h, 0.35) || !near(w.Rate6h, 0.35/6) || !near(w.Rate24h, 0.35/24) {
		t.Errorf("rates = %v %v %v", w.Rate1h, w.Rate6h, w.Rate24h)
	}
	// f = (now − (resets_at − 5h)) / 5h = 1h / 5h.
	if !near(w.FractionElapsed, 0.2) {
		t.Errorf("fraction elapsed = %v", w.FractionElapsed)
	}
	// target_rate = (0.9 − 0.25) / 4h; delta = r(1h) − target_rate.
	if !near(w.TargetRate, 0.1625) || !near(w.Delta, 0.35-0.1625) {
		t.Errorf("target rate %v delta %v", w.TargetRate, w.Delta)
	}
	// forecast = u + r(1h)·hours_to_reset (+ 0 queued).
	if !near(w.ForecastAtReset, 0.25+0.35*4) {
		t.Errorf("forecast = %v", w.ForecastAtReset)
	}
	if w.LastResetUnspent == nil || !near(*w.LastResetUnspent, 0.9-0.8) {
		t.Errorf("last reset unspent = %v", w.LastResetUnspent)
	}
	// No samples for seven_day: utilization −1 and everything else zero.
	s := u.SevenDay
	if s.Utilization != -1 || s.Rate1h != 0 || s.TargetRate != 0 || s.FractionElapsed != 0 || s.LastResetUnspent != nil || s.ForecastAtReset != 0 {
		t.Errorf("empty window: %+v", s)
	}
}

func TestComputeUsagePastReset(t *testing.T) {
	now := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	samples := map[string][]store.RateLimitSample{"seven_day": {
		sample(now.Add(-2*time.Hour), "seven_day", 0.5, now.Add(-time.Hour)),
	}}
	w := ComputeUsage(now, budgetCfg, samples, nil, nil, nil).SevenDay
	// Past the reset: hours_to_reset floors at 0, f clamps to 1.
	if w.TargetRate != 0 || !near(w.FractionElapsed, 1) || !near(w.ForecastAtReset, 0.5) {
		t.Errorf("past reset: %+v", w)
	}
}

func TestComputeUsageCalibration(t *testing.T) {
	now := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	fact := func(in, out, cc int64, delta *float64) store.AttemptFacts {
		return store.AttemptFacts{Routine: "r", FinishedAt: now, InputTokens: &in, OutputTokens: &out, CacheCreation: &cc, UtilizationDelta: delta}
	}
	odd := []store.AttemptFacts{
		fact(100, 50, 50, fp(0.02)), // 200/0.02 = 10000
		fact(400, 0, 0, fp(0.02)),   // 20000
		fact(600, 0, 0, fp(0.02)),   // 30000
		fact(999, 0, 0, nil),        // NULL delta ignored
		fact(999, 0, 0, fp(0)),      // zero delta ignored
	}
	if got := ComputeUsage(now, budgetCfg, nil, odd, nil, nil).TokensPerPoint; !near(got, 20000) {
		t.Errorf("odd median = %v", got)
	}
	even := append(odd, fact(800, 0, 0, fp(0.02))) // adds 40000
	if got := ComputeUsage(now, budgetCfg, nil, even, nil, nil).TokensPerPoint; !near(got, 25000) {
		t.Errorf("even median = %v", got)
	}
}

func TestComputeUsageQueuedEstimate(t *testing.T) {
	now := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	byRoutine := map[string][]store.AttemptFacts{
		"a": {
			{Routine: "a", UtilizationDelta: fp(0.01)},
			{Routine: "a", UtilizationDelta: fp(0.03)},
			{Routine: "a", UtilizationDelta: fp(0.02)},
		},
		"c": {{Routine: "c"}}, // only NULL deltas → no estimate
	}
	samples := map[string][]store.RateLimitSample{"five_hour": {
		sample(now.Add(-time.Minute), "five_hour", 0.4, now.Add(2*time.Hour)),
	}}
	// Two queued "a" at median 0.02 each; "b" and "c" have no estimate.
	u := ComputeUsage(now, budgetCfg, samples, nil, []string{"a", "a", "b", "c"}, byRoutine)
	if !near(u.QueuedEstimate, 0.04) || u.QueuedUnknown != 2 {
		t.Errorf("queued = %v unknown %d", u.QueuedEstimate, u.QueuedUnknown)
	}
	// The forecast carries the queued estimate.
	if !near(u.FiveHour.ForecastAtReset, 0.4+0.04) {
		t.Errorf("forecast with queue = %v", u.FiveHour.ForecastAtReset)
	}
}

func TestComputeUsageDailyUSD(t *testing.T) {
	// 01:00 local in UTC−4: local midnight is 04:00 UTC.
	zone := time.FixedZone("test", -4*3600)
	now := time.Date(2026, 8, 30, 1, 0, 0, 0, zone)
	facts := []store.AttemptFacts{
		{FinishedAt: now.Add(-30 * time.Minute), CostUSD: fp(1.5)}, // today (local)
		{FinishedAt: now.Add(-2 * time.Hour), CostUSD: fp(2.0)},    // yesterday (local)
		{FinishedAt: now.Add(-10 * time.Minute)},                   // no cost
	}
	if got := ComputeUsage(now, budgetCfg, nil, facts, nil, nil).DailyUSD; !near(got, 1.5) {
		t.Errorf("daily usd = %v", got)
	}
}

// dwin builds one window's usage for the Decide table: utilization u, 1 h rate,
// hours to reset, and fraction elapsed. u < 0 means "no samples yet".
func dwin(now time.Time, name string, u, rate1h, hoursToReset, f float64) WindowUsage {
	w := WindowUsage{Window: name, Utilization: u, Rate1h: rate1h, FractionElapsed: f}
	if u >= 0 {
		w.ResetsAt = now.Add(time.Duration(hoursToReset * float64(time.Hour)))
	}
	return w
}

func TestDecide(t *testing.T) {
	noon := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	morning := time.Date(2026, 8, 30, 8, 0, 0, 0, time.UTC)
	night := time.Date(2026, 8, 30, 23, 0, 0, 0, time.UTC)
	early := time.Date(2026, 8, 30, 5, 0, 0, 0, time.UTC)
	cfgCap := budgetCfg
	cfgCap.DailyUSDCap = 25
	cfgQuiet := budgetCfg
	cfgQuiet.QuietHours = QuietHoursConfig{Start: "09:00", End: "18:00", Reserve: 0.2}
	cfgWrap := budgetCfg
	cfgWrap.QuietHours = QuietHoursConfig{Start: "22:00", End: "06:00", Reserve: 0.2}
	clearFive := dwin(noon, "five_hour", 0.3, 0.05, 2, 0.5)
	noSeven := dwin(noon, "seven_day", -1, 0, 0, 0)
	cases := []struct {
		name   string
		now    time.Time
		cfg    BudgetConfig
		u      Usage
		class  model.BudgetClass
		admit  bool
		reason string
	}{
		{"hard stop five_hour interactive", noon, budgetCfg, Usage{FiveHour: dwin(noon, "five_hour", 0.97, 0, 2, 0.5), SevenDay: noSeven}, model.ClassInteractive, false, "hard_stop:five_hour"},
		{"hard stop five_hour normal", noon, budgetCfg, Usage{FiveHour: dwin(noon, "five_hour", 0.97, 0, 2, 0.5), SevenDay: noSeven}, model.ClassNormal, false, "hard_stop:five_hour"},
		{"hard stop five_hour backlog", noon, budgetCfg, Usage{FiveHour: dwin(noon, "five_hour", 0.97, 0, 2, 0.5), SevenDay: noSeven}, model.ClassBacklog, false, "hard_stop:five_hour"},
		{"hard stop seven_day", noon, budgetCfg, Usage{FiveHour: clearFive, SevenDay: dwin(noon, "seven_day", 0.98, 0, 100, 0.5)}, model.ClassNormal, false, "hard_stop:seven_day"},
		{"daily cap blocks interactive", noon, cfgCap, Usage{FiveHour: clearFive, SevenDay: noSeven, DailyUSD: 25}, model.ClassInteractive, false, "daily_usd_cap"},
		{"daily cap blocks normal", noon, cfgCap, Usage{FiveHour: clearFive, SevenDay: noSeven, DailyUSD: 26}, model.ClassNormal, false, "daily_usd_cap"},
		{"daily under cap", noon, cfgCap, Usage{FiveHour: clearFive, SevenDay: noSeven, DailyUSD: 24.5}, model.ClassInteractive, true, ""},
		{"no cap ignores spend", noon, budgetCfg, Usage{FiveHour: clearFive, SevenDay: noSeven, DailyUSD: 1000}, model.ClassNormal, true, ""},
		{"both clear normal", noon, budgetCfg, Usage{FiveHour: clearFive, SevenDay: dwin(noon, "seven_day", 0.2, 0.001, 100, 0.5)}, model.ClassNormal, true, ""},
		{"over target interactive admits", noon, budgetCfg, Usage{FiveHour: dwin(noon, "five_hour", 0.92, 0, 2, 0.5), SevenDay: noSeven}, model.ClassInteractive, true, ""},
		{"over target normal", noon, budgetCfg, Usage{FiveHour: dwin(noon, "five_hour", 0.92, 0, 2, 0.5), SevenDay: noSeven}, model.ClassNormal, false, "over_target:five_hour"},
		{"over target backlog", noon, budgetCfg, Usage{FiveHour: dwin(noon, "five_hour", 0.92, 0, 2, 0.5), SevenDay: noSeven}, model.ClassBacklog, false, "over_target:five_hour"},
		{"forecast over five_hour", noon, budgetCfg, Usage{FiveHour: dwin(noon, "five_hour", 0.5, 0.2, 3, 0.5), SevenDay: noSeven}, model.ClassNormal, false, "forecast_over_target:five_hour"},
		{"forecast over seven_day", noon, budgetCfg, Usage{FiveHour: clearFive, SevenDay: dwin(noon, "seven_day", 0.5, 0.005, 100, 0.5)}, model.ClassNormal, false, "forecast_over_target:seven_day"},
		{"quiet hours blocks normal", noon, cfgQuiet, Usage{FiveHour: dwin(noon, "five_hour", 0.75, 0, 1, 0.9), SevenDay: noSeven}, model.ClassNormal, false, "quiet_hours"},
		{"quiet hours blocks backlog", noon, cfgQuiet, Usage{FiveHour: dwin(noon, "five_hour", 0.75, 0, 1, 0.9), SevenDay: noSeven}, model.ClassBacklog, false, "quiet_hours"},
		{"quiet hours spares interactive", noon, cfgQuiet, Usage{FiveHour: dwin(noon, "five_hour", 0.75, 0, 1, 0.9), SevenDay: noSeven}, model.ClassInteractive, true, ""},
		{"quiet hours below reserve", noon, cfgQuiet, Usage{FiveHour: dwin(noon, "five_hour", 0.65, 0, 1, 0.9), SevenDay: noSeven}, model.ClassNormal, true, ""},
		{"outside quiet hours", morning, cfgQuiet, Usage{FiveHour: dwin(morning, "five_hour", 0.75, 0, 1, 0.9), SevenDay: noSeven}, model.ClassNormal, true, ""},
		{"wrapping quiet hours before midnight", night, cfgWrap, Usage{FiveHour: dwin(night, "five_hour", 0.75, 0, 1, 0.9), SevenDay: noSeven}, model.ClassNormal, false, "quiet_hours"},
		{"wrapping quiet hours after midnight", early, cfgWrap, Usage{FiveHour: dwin(early, "five_hour", 0.75, 0, 1, 0.9), SevenDay: noSeven}, model.ClassNormal, false, "quiet_hours"},
		{"wrapping quiet hours daytime", noon, cfgWrap, Usage{FiveHour: dwin(noon, "five_hour", 0.75, 0, 1, 0.9), SevenDay: noSeven}, model.ClassNormal, true, ""},
		{"backlog ahead of burn-down early", noon, budgetCfg, Usage{FiveHour: dwin(noon, "five_hour", 0.2, 0, 3, 0.1), SevenDay: noSeven}, model.ClassBacklog, false, "ahead_of_burn_down_line:five_hour"},
		{"backlog behind burn-down early", noon, budgetCfg, Usage{FiveHour: dwin(noon, "five_hour", 0.045, 0, 3, 0.1), SevenDay: noSeven}, model.ClassBacklog, true, ""},
		{"backlog admitted late in window", noon, budgetCfg, Usage{FiveHour: dwin(noon, "five_hour", 0.5, 0, 3, 0.95), SevenDay: noSeven}, model.ClassBacklog, true, ""},
		{"backlog seven_day line", noon, budgetCfg, Usage{FiveHour: dwin(noon, "five_hour", 0.01, 0, 3, 0.5), SevenDay: dwin(noon, "seven_day", 0.5, 0, 24, 0.1)}, model.ClassBacklog, false, "ahead_of_burn_down_line:seven_day"},
		{"no samples interactive", noon, budgetCfg, Usage{FiveHour: dwin(noon, "five_hour", -1, 0, 0, 0), SevenDay: noSeven}, model.ClassInteractive, true, ""},
		{"no samples normal", noon, budgetCfg, Usage{FiveHour: dwin(noon, "five_hour", -1, 0, 0, 0), SevenDay: noSeven}, model.ClassNormal, true, ""},
		{"no samples backlog", noon, budgetCfg, Usage{FiveHour: dwin(noon, "five_hour", -1, 0, 0, 0), SevenDay: noSeven}, model.ClassBacklog, true, ""},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			admit, reason := Decide(tc.now, tc.u, tc.class, tc.cfg)
			if admit != tc.admit || reason != tc.reason {
				t.Errorf("Decide = %v %q, want %v %q", admit, reason, tc.admit, tc.reason)
			}
		})
	}
}

const budgetWorkerID = "fedcba9876543210fedcba9876543210"

// budgetStore builds the minimal daemon-side world for policy tests: a real
// store with one project, one worker with one repository, and one routine.
func budgetStore(t *testing.T, clock func() time.Time) *store.Store {
	t.Helper()
	st, err := store.Open(bctx(), filepath.Join(t.TempDir(), "forge.sqlite3"), store.Options{Clock: clock})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	})
	err = st.Write(bctx(), func(tx *store.Tx) error {
		if err := tx.EnsureProject(bctx(), "default"); err != nil {
			return err
		}
		if err := tx.Register(bctx(), protocol.RegisterRequest{WorkerID: budgetWorkerID, Name: "laptop", Version: "test", MaxConcurrent: 2, Executors: []string{"claude-code"},
			Repositories: []protocol.Repository{{Name: "equitizr", Path: "/tmp/equitizr", OriginIdentity: "github.com/x/equitizr"}}}); err != nil {
			return err
		}
		return tx.CreateRoutine(bctx(), &store.Routine{Name: "inventory", Mode: "run", Prompt: "list files", Repositories: []string{"equitizr"}, Model: "haiku", TimeoutSeconds: 300})
	})
	if err != nil {
		t.Fatal(err)
	}
	return st
}

func budgetWork(t *testing.T, st *store.Store, routine, title string) (*store.Work, store.Target) {
	t.Helper()
	w := &store.Work{RoutineName: routine, Generation: 1, Title: title, Trigger: model.TriggerManual, Snapshot: []byte(`{}`), Priority: 100, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto}
	var targets []store.Target
	err := st.Write(bctx(), func(tx *store.Tx) error {
		var err error
		targets, err = tx.CreateWork(bctx(), w, []string{"equitizr"}, nil)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return w, targets[0]
}

func TestBudgetPolicy(t *testing.T) {
	now0 := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	now := now0
	clock := func() time.Time { return now }
	st := budgetStore(t, clock)

	// One claimed Work whose finished attempt calibrates the routine.
	_, claimed := budgetWork(t, st, "inventory", "claimed")
	var attempt *store.Attempt
	err := st.Write(bctx(), func(tx *store.Tx) error {
		var err error
		attempt, err = tx.Claim(bctx(), store.ClaimParams{TargetID: claimed.ID, WorkerID: budgetWorkerID, ClaimRequestID: "c1", LeaseToken: "l1", MCPToken: "m1",
			Executor: "claude-code", Model: "claude-haiku-4-5", ModelAlias: "haiku", Mode: "run", Autonomy: model.AutonomyAuto})
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	in, out, cc := int64(100), int64(50), int64(25)
	err = st.Write(bctx(), func(tx *store.Tx) error {
		if err := tx.InsertFacts(bctx(), &store.AttemptFacts{AttemptID: attempt.ID, TargetID: claimed.ID, WorkID: claimed.WorkID, Routine: "inventory", Generation: 1,
			Project: "default", Repository: "equitizr", Worker: budgetWorkerID, Executor: "claude-code", Model: "haiku", Mode: "run", Trigger: model.TriggerManual,
			Autonomy: model.AutonomyAuto, FinishedAt: now0, State: model.Succeeded,
			InputTokens: &in, OutputTokens: &out, CacheCreation: &cc, CostUSD: fp(1.25), UtilizationDelta: fp(0.02)}); err != nil {
			return err
		}
		return tx.InsertSamples(bctx(), []store.RateLimitSample{
			sample(now0.Add(-time.Minute), "five_hour", 0.2, now0.Add(3*time.Hour)),
			sample(now0.Add(-time.Minute), "seven_day", 0.3, now0.Add(100*time.Hour)),
		})
	})
	if err != nil {
		t.Fatal(err)
	}
	// Two queued Works: one with an estimate, one without.
	budgetWork(t, st, "inventory", "queued")
	budgetWork(t, st, "mystery", "queued unknown")

	p := NewBudgetPolicy(st, budgetCfg, clock)
	u, err := p.Usage(bctx())
	if err != nil {
		t.Fatal(err)
	}
	if u.FiveHour.Utilization != 0.2 || u.SevenDay.Utilization != 0.3 {
		t.Errorf("utilization = %v %v", u.FiveHour.Utilization, u.SevenDay.Utilization)
	}
	if !near(u.QueuedEstimate, 0.02) || u.QueuedUnknown != 1 {
		t.Errorf("queued = %v unknown %d", u.QueuedEstimate, u.QueuedUnknown)
	}
	if !near(u.DailyUSD, 1.25) || !near(u.TokensPerPoint, 175/0.02) {
		t.Errorf("daily %v tokens/point %v", u.DailyUSD, u.TokensPerPoint)
	}

	// Decide loads and caches at now0.
	if admit, reason := p.Decide(model.ClassNormal); !admit || reason != "" {
		t.Fatalf("Decide = %v %q, want admit", admit, reason)
	}
	// New over-target sample lands after the cache was filled.
	err = st.Write(bctx(), func(tx *store.Tx) error {
		return tx.InsertSamples(bctx(), []store.RateLimitSample{sample(now0, "five_hour", 0.95, now0.Add(3*time.Hour))})
	})
	if err != nil {
		t.Fatal(err)
	}
	now = now0.Add(5 * time.Second)
	if admit, reason := p.Decide(model.ClassNormal); !admit || reason != "" {
		t.Errorf("within the cache TTL Decide must not reload: %v %q", admit, reason)
	}
	// The uncached load sees the new sample immediately.
	if u, err := p.Usage(bctx()); err != nil || u.FiveHour.Utilization != 0.95 {
		t.Errorf("uncached usage = %+v, %v", u.FiveHour, err)
	}
	now = now0.Add(11 * time.Second)
	if admit, reason := p.Decide(model.ClassNormal); admit || reason != "over_target:five_hour" {
		t.Errorf("after the TTL Decide = %v %q, want over_target:five_hour", admit, reason)
	}
}

func TestBudgetResets(t *testing.T) {
	now := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	resetsA := now.Add(time.Hour)
	resetsB := now.Add(6 * time.Hour)

	// Pure detection: within-batch boundary with no stored history.
	prev := map[string]*store.RateLimitSample{}
	batch := []store.RateLimitSample{
		sample(now.Add(-time.Minute), "five_hour", 0.5, resetsA),
		sample(now, "five_hour", 0.05, resetsB),
	}
	got := budgetResets(budgetCfg, prev, batch)
	if len(got) != 1 || got[0].Window != "five_hour" || !near(got[0].Unspent, 0.9-0.5) || !got[0].PrevResetsAt.Equal(resetsA) || !got[0].NewResetsAt.Equal(resetsB) {
		t.Errorf("in-batch boundary = %+v", got)
	}
	// A stored previous sample against one incoming sample.
	prevSample := sample(now.Add(-time.Hour), "five_hour", 0.8, resetsA)
	got = budgetResets(budgetCfg, map[string]*store.RateLimitSample{"five_hour": &prevSample}, []store.RateLimitSample{sample(now, "five_hour", 0.1, resetsB)})
	if len(got) != 1 || !near(got[0].Unspent, 0.9-0.8) {
		t.Errorf("stored boundary = %+v", got)
	}
	// Same resets_at, and no prior sample at all: nothing.
	if got := budgetResets(budgetCfg, map[string]*store.RateLimitSample{"five_hour": &prevSample}, []store.RateLimitSample{sample(now, "five_hour", 0.9, resetsA)}); len(got) != 0 {
		t.Errorf("no boundary = %+v", got)
	}
	if got := budgetResets(budgetCfg, prev, []store.RateLimitSample{sample(now, "seven_day", 0.1, resetsB)}); len(got) != 0 {
		t.Errorf("first-ever sample journals nothing: %+v", got)
	}
}

func TestJournalBudgetResets(t *testing.T) {
	now := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	clock := func() time.Time { return now }
	st := budgetStore(t, clock)
	resetsA := now.Add(time.Hour)
	resetsB := now.Add(6 * time.Hour)
	err := st.Write(bctx(), func(tx *store.Tx) error {
		return tx.InsertSamples(bctx(), []store.RateLimitSample{sample(now.Add(-time.Hour), "five_hour", 0.8, resetsA)})
	})
	if err != nil {
		t.Fatal(err)
	}
	srv, err := NewServer(ServerOptions{Store: st, Policy: NewBudgetPolicy(st, budgetCfg, clock)})
	if err != nil {
		t.Fatal(err)
	}
	incoming := []store.RateLimitSample{sample(now, "five_hour", 0.1, resetsB)}
	err = st.Write(bctx(), func(tx *store.Tx) error {
		if err := srv.journalBudgetResets(bctx(), tx, incoming); err != nil {
			return err
		}
		return tx.InsertSamples(bctx(), incoming)
	})
	if err != nil {
		t.Fatal(err)
	}
	entries, err := st.JournalForEntity(bctx(), store.EntityDaemon, "budget")
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 1 || entries[0].Kind != "budget.reset" {
		t.Fatalf("journal = %+v", entries)
	}
	var payload budgetReset
	if err := json.Unmarshal(entries[0].Payload, &payload); err != nil {
		t.Fatal(err)
	}
	if payload.Window != "five_hour" || !near(payload.Unspent, 0.9-0.8) || !payload.PrevResetsAt.Equal(resetsA) || !payload.NewResetsAt.Equal(resetsB) {
		t.Errorf("payload = %+v", payload)
	}
	// A first-ever seven_day sample has no prior boundary to journal.
	err = st.Write(bctx(), func(tx *store.Tx) error {
		first := []store.RateLimitSample{sample(now, "seven_day", 0.2, resetsB)}
		if err := srv.journalBudgetResets(bctx(), tx, first); err != nil {
			return err
		}
		return tx.InsertSamples(bctx(), first)
	})
	if err != nil {
		t.Fatal(err)
	}
	// Without the budget policy (AdmitAll lacks the capability) nothing is journaled.
	plain, err := NewServer(ServerOptions{Store: st, Policy: AdmitAll{}})
	if err != nil {
		t.Fatal(err)
	}
	err = st.Write(bctx(), func(tx *store.Tx) error {
		return plain.journalBudgetResets(bctx(), tx, []store.RateLimitSample{sample(now.Add(time.Minute), "five_hour", 0.05, now.Add(11*time.Hour))})
	})
	if err != nil {
		t.Fatal(err)
	}
	entries, err = st.JournalForEntity(bctx(), store.EntityDaemon, "budget")
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 1 {
		t.Errorf("journal grew unexpectedly: %+v", entries)
	}
}
