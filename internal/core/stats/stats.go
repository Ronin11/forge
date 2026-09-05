// Package stats is the one home for the DESIGN.md §9.4 aggregation over
// attempt facts: per-routine and per-generation outcomes, rates, duration
// percentiles, tokens, cost, and the previous-window comparison, plus the
// retro data pack. It imports only model and store so both internal/tools
// (the MCP surface) and internal/controlplane (the HTTP surface) can share
// it without a cycle. Percentiles are computed in Go over rows from an
// indexed range query (STYLE.md §4), never in SQL.
package stats

import (
	"context"
	"encoding/json"
	"fmt"
	"math"
	"sort"
	"strconv"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// SchemaVersion stamps the retro pack so its consumer (the M5 retro mode)
// can detect shape changes.
const SchemaVersion = 1

// Retro pack bounds (DESIGN.md §9.4).
const (
	// problemCap keeps the pack bounded: only the newest problem attempts.
	problemCap = 200
	// eventTail is how many trailing non-line events each problem attempt
	// carries — the design's "last 30 non-stdout events".
	eventTail = 30
	// resultTextCap bounds each problem attempt's free-text result (bytes).
	resultTextCap = 2000
)

// Query is one stats window with its optional filters. Routine narrows the
// facts fetch (it is indexed); Repository, Project, and Mode are filtered in
// Go because every facts row carries them.
type Query struct {
	Since      time.Time `json:"since"`
	Until      time.Time `json:"until"`
	Routine    string    `json:"routine,omitempty"`
	Repository string    `json:"repository,omitempty"`
	Project    string    `json:"project,omitempty"`
	Mode       string    `json:"mode,omitempty"`
	// Size narrows to one bucket (S|M|L, or "unsized" for rows without one).
	Size string `json:"size,omitempty"`
}

// RoutineStats is one routine's (or one routine generation's) aggregate over
// a window. NULL facts columns never contribute to a sum or a denominator:
//
//   - VerifiedSuccessRate = VerifiedSuccesses / Runs. Every run counts in the
//     denominator — an attempt whose verification outcome is unknown counts
//     against the rate, because "verified" is the claim being measured.
//   - SelfReportedSuccessRate's denominator is only the runs whose is_error
//     is non-NULL (the executor reported an outcome); the numerator is those
//     with is_error = false. Zero when no run reported.
//   - TokensInPerRun / TokensOutPerRun / TurnsPerRun / QuestionsPerRun are
//     means over the runs where the column is non-NULL; zero when none is.
//   - CostPerRun = CostUSDTotal / runs with a non-NULL cost_usd (a run whose
//     cost is unknown is excluded from the denominator, not counted as free).
//   - The duration fields are nearest-rank percentiles over the runs whose
//     phase value is non-NULL; zero when no run carried the phase.
//
// The l1_vacuous marker is not recorded in attempt_facts (only
// verification_level and verification_passed are), so a "verified but
// vacuous" count cannot be computed here; it is deliberately absent.
type RoutineStats struct {
	Routine    string `json:"routine"`
	Generation int    `json:"generation"` // 0 = all generations rolled up

	Runs     int            `json:"runs"`
	Outcomes map[string]int `json:"outcomes"` // state → count

	// VerifiedSuccesses counts runs in an accepted state (model.IsSuccess,
	// the one definition) whose verification_passed is recorded true; a NULL
	// verification_passed is not a pass.
	VerifiedSuccesses       int     `json:"verified_successes"`
	VerifiedSuccessRate     float64 `json:"verified_success_rate"`
	SelfReportedSuccessRate float64 `json:"self_reported_success_rate"`

	P50TotalUS int64 `json:"p50_total_us"`
	P95TotalUS int64 `json:"p95_total_us"`
	MaxTotalUS int64 `json:"max_total_us"`
	P50AgentUS int64 `json:"p50_agent_us"`
	P95AgentUS int64 `json:"p95_agent_us"`
	MaxAgentUS int64 `json:"max_agent_us"`

	TokensInPerRun  float64 `json:"tokens_in_per_run"`
	TokensOutPerRun float64 `json:"tokens_out_per_run"`

	CostUSDTotal float64 `json:"cost_usd_total"`
	CostPerRun   float64 `json:"cost_per_run"`
	// CostPerVerifiedSuccess is nil when the window has no verified success —
	// the ratio is undefined, not zero.
	CostPerVerifiedSuccess *float64 `json:"cost_per_verified_success"`

	TurnsPerRun     float64 `json:"turns_per_run"`
	QuestionsPerRun float64 `json:"questions_per_run"`

	ToolMix           map[string]int `json:"tool_mix"`            // tool_calls_by_name summed
	TopFailureReasons []ReasonCount  `json:"top_failure_reasons"` // count desc, capped 5

	// UtilizationConsumed sums the positive utilization_delta_estimate values;
	// NULL and negative deltas (a reset boundary) are skipped.
	UtilizationConsumed float64 `json:"utilization_consumed"`
}

// ReasonCount is one failure reason's tally.
type ReasonCount struct {
	Reason string `json:"reason"`
	Count  int    `json:"count"`
}

// Report is the whole §9.4 answer for one window: the per-routine rollups,
// the per-generation split, and the previous equal window for deltas.
type Report struct {
	Query       Query          `json:"query"`
	Routines    []RoutineStats `json:"routines"`    // Generation 0, sorted by routine
	Generations []RoutineStats `json:"generations"` // sorted by routine, then generation
	// Prev maps Key(routine, generation) — "routine" for the rollup,
	// "routine@N" per generation — to the same aggregate computed over the
	// previous equal window [Since−(Until−Since), Since). Routines with no
	// runs in the previous window have no entry.
	Prev      map[string]RoutineStats `json:"prev"`
	TotalRuns int                     `json:"total_runs"`
	// Funnel is the proposal funnel (DESIGN.md §12). Proposals are decided
	// once, not windowed like facts, so Store.Funnel counts them all-time and
	// every window reports the same funnel. Nil when the report came from
	// Compute alone (which never touches the store).
	Funnel *store.ProposalFunnel `json:"funnel,omitempty"`
	// CostPerApplied is the window's reflection spend (facts with mode
	// "retro", after the query's filters) divided by the all-time applied
	// count; nil while nothing has been applied — undefined, not zero.
	CostPerApplied *float64 `json:"cost_per_applied,omitempty"`
	// Matrix is the M10 capability matrix (DESIGN.md §21): one row per model
	// (its class the capability axis), with verified success and cost per
	// verified success in each currency. Runners is per-runner utilization.
	Matrix  []ModelCapability   `json:"matrix"`
	Runners []RunnerUtilization `json:"runners"`
	// Sizes is the size-calibration table (S/M/L/unsized): cost, turns, and
	// mean overall score per bucket — whether the sizing is honest.
	Sizes []SizeStats `json:"sizes,omitempty"`
}

// ModelCapability is one model's row in the capability matrix: how often it
// verified and what each verified success cost, notional and in subscription
// budget (M10, DESIGN.md §21).
type ModelCapability struct {
	Model               string   `json:"model"`
	Class               string   `json:"class"`
	Runner              string   `json:"runner"`
	Runs                int      `json:"runs"`
	Trials              int      `json:"trials"` // attempts whose verification was decided
	VerifiedSuccesses   int      `json:"verified_successes"`
	VerifiedSuccessRate float64  `json:"verified_success_rate"`
	CostPerVerifiedUSD  *float64 `json:"cost_per_verified_usd"`       // nil when no verified success
	FiveHourPerVerified *float64 `json:"cost_per_verified_five_hour"` // subscription-window points
	SevenDayPerVerified *float64 `json:"cost_per_verified_seven_day"`
	AvgRunnerSeconds    float64  `json:"avg_runner_seconds"`
}

// RunnerUtilization is one runner's totals across the window.
type RunnerUtilization struct {
	Runner             string  `json:"runner"`
	Runs               int     `json:"runs"`
	TotalRunnerSeconds float64 `json:"total_runner_seconds"`
	TotalUSD           float64 `json:"total_usd"`
}

// Key names a RoutineStats row in Report.Prev: the bare routine for the
// all-generations rollup, "routine@N" for one generation.
func Key(routine string, generation int) string {
	if generation == 0 {
		return routine
	}
	return routine + "@" + strconv.Itoa(generation)
}

// Compute is the pure aggregation: current becomes Routines/Generations,
// prev becomes Prev. It never touches a store, so tests feed it literals.
// The caller fills Report.Query (Load does).
func Compute(current, prev []store.AttemptFacts) *Report {
	r := &Report{
		Routines:    aggregate(current, false),
		Generations: aggregate(current, true),
		Prev:        map[string]RoutineStats{},
		TotalRuns:   len(current),
	}
	for _, s := range aggregate(prev, false) {
		r.Prev[Key(s.Routine, 0)] = s
	}
	for _, s := range aggregate(prev, true) {
		r.Prev[Key(s.Routine, s.Generation)] = s
	}
	r.Matrix = capabilityMatrix(current)
	r.Runners = runnerUtilization(current)
	r.Sizes = sizeBuckets(current)
	return r
}

// SizeStats is one size bucket's calibration row: whether "S actually costs
// S". ScoreOverallMean averages only the rows that carried a score; nil when
// none did.
type SizeStats struct {
	Size                string   `json:"size"` // S, M, L, or "unsized"
	Runs                int      `json:"runs"`
	VerifiedSuccesses   int      `json:"verified_successes"`
	VerifiedSuccessRate float64  `json:"verified_success_rate"`
	CostUSDTotal        float64  `json:"cost_usd_total"`
	CostPerRun          float64  `json:"cost_per_run"`
	TurnsPerRun         float64  `json:"turns_per_run"`
	DurationP50US       int64    `json:"duration_p50_us"`
	ScoreOverallMean    *float64 `json:"score_overall_mean,omitempty"`
}

// sizeBuckets groups the window by the work's size bucket. Order is fixed
// (S, M, L, unsized); empty buckets are omitted.
func sizeBuckets(rows []store.AttemptFacts) []SizeStats {
	type acc struct {
		SizeStats
		costRuns, turnRuns int
		turns              int
		totals             []int64
		scoreSum, scoreN   int
	}
	byBucket := map[string]*acc{}
	for i := range rows {
		f := &rows[i]
		bucket := f.Size
		if bucket == "" {
			bucket = "unsized"
		}
		a := byBucket[bucket]
		if a == nil {
			a = &acc{SizeStats: SizeStats{Size: bucket}}
			byBucket[bucket] = a
		}
		a.Runs++
		if model.IsSuccess(f.State) && f.VerificationPass != nil && *f.VerificationPass {
			a.VerifiedSuccesses++
		}
		if f.CostUSD != nil {
			a.CostUSDTotal += *f.CostUSD
			a.costRuns++
		}
		if f.Turns != nil {
			a.turns += *f.Turns
			a.turnRuns++
		}
		if v := f.Phases["total"]; v != nil {
			a.totals = append(a.totals, *v)
		}
		if f.ScoreOverall != nil {
			a.scoreSum += *f.ScoreOverall
			a.scoreN++
		}
	}
	var out []SizeStats
	for _, bucket := range []string{"S", "M", "L", "unsized"} {
		a := byBucket[bucket]
		if a == nil {
			continue
		}
		a.VerifiedSuccessRate = float64(a.VerifiedSuccesses) / float64(a.Runs)
		if a.costRuns > 0 {
			a.CostPerRun = a.CostUSDTotal / float64(a.costRuns)
		}
		if a.turnRuns > 0 {
			a.TurnsPerRun = float64(a.turns) / float64(a.turnRuns)
		}
		a.DurationP50US, _, _ = percentiles(a.totals)
		if a.scoreN > 0 {
			mean := float64(a.scoreSum) / float64(a.scoreN)
			a.ScoreOverallMean = &mean
		}
		out = append(out, a.SizeStats)
	}
	return out
}

// capabilityMatrix groups the window's facts by model into the M10 capability
// matrix. Cost-per-verified-success is the summed cost over the verified
// successes; nil when a model has none, so an unproven model reads as "no
// cost-per-success yet" rather than a misleading zero.
func capabilityMatrix(rows []store.AttemptFacts) []ModelCapability {
	type acc struct {
		class, runner                    string
		runs, trials, verified           int
		usd, fiveHour, sevenDay, seconds float64
		usdN, fiveHourN, sevenDayN       int
	}
	byModel := map[string]*acc{}
	for _, f := range rows {
		if f.Model == "" {
			continue
		}
		a := byModel[f.Model]
		if a == nil {
			a = &acc{}
			byModel[f.Model] = a
		}
		a.runs++
		if f.ModelClass != "" {
			a.class = f.ModelClass
		}
		if f.Runner != "" {
			a.runner = f.Runner
		}
		verified := model.IsSuccess(f.State) && f.VerificationPass != nil && *f.VerificationPass
		if f.VerificationPass != nil {
			a.trials++
		}
		if verified {
			a.verified++
			if f.USD != nil {
				a.usd += *f.USD
				a.usdN++
			}
			if f.FiveHourDelta != nil {
				a.fiveHour += *f.FiveHourDelta
				a.fiveHourN++
			}
			if f.SevenDayDelta != nil {
				a.sevenDay += *f.SevenDayDelta
				a.sevenDayN++
			}
		}
		if f.RunnerSeconds != nil {
			a.seconds += *f.RunnerSeconds
		}
	}
	out := make([]ModelCapability, 0, len(byModel))
	for name, a := range byModel {
		m := ModelCapability{Model: name, Class: a.class, Runner: a.runner, Runs: a.runs, Trials: a.trials, VerifiedSuccesses: a.verified}
		m.VerifiedSuccessRate = ratio(a.verified, a.trials)
		if a.runs > 0 {
			m.AvgRunnerSeconds = a.seconds / float64(a.runs)
		}
		if a.verified > 0 {
			if a.usdN > 0 {
				v := a.usd / float64(a.verified)
				m.CostPerVerifiedUSD = &v
			}
			if a.fiveHourN > 0 {
				v := a.fiveHour / float64(a.verified)
				m.FiveHourPerVerified = &v
			}
			if a.sevenDayN > 0 {
				v := a.sevenDay / float64(a.verified)
				m.SevenDayPerVerified = &v
			}
		}
		out = append(out, m)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Model < out[j].Model })
	return out
}

// runnerUtilization sums runner_seconds and notional usd per runner.
func runnerUtilization(rows []store.AttemptFacts) []RunnerUtilization {
	type acc struct {
		runs    int
		seconds float64
		usd     float64
	}
	byRunner := map[string]*acc{}
	for _, f := range rows {
		if f.Runner == "" {
			continue
		}
		a := byRunner[f.Runner]
		if a == nil {
			a = &acc{}
			byRunner[f.Runner] = a
		}
		a.runs++
		if f.RunnerSeconds != nil {
			a.seconds += *f.RunnerSeconds
		}
		if f.USD != nil {
			a.usd += *f.USD
		}
	}
	out := make([]RunnerUtilization, 0, len(byRunner))
	for name, a := range byRunner {
		out = append(out, RunnerUtilization{Runner: name, Runs: a.runs, TotalRunnerSeconds: a.seconds, TotalUSD: a.usd})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Runner < out[j].Runner })
	return out
}

// Load fetches the window and its previous equal window with the indexed
// facts query, applies the Go-side filters, computes the report, and attaches
// the proposal funnel with its cost-per-applied.
func Load(ctx context.Context, st *store.Store, q Query) (*Report, error) {
	current, prev, err := loadFacts(ctx, st, q)
	if err != nil {
		return nil, err
	}
	r := Compute(current, prev)
	r.Query = q
	funnel, err := st.Funnel(ctx)
	if err != nil {
		return nil, fmt.Errorf("load proposal funnel: %w", err)
	}
	r.Funnel = &funnel
	if funnel.Applied > 0 {
		var retro float64
		for i := range current {
			if current[i].Mode == "retro" && current[i].CostUSD != nil {
				retro += *current[i].CostUSD
			}
		}
		v := retro / float64(funnel.Applied)
		r.CostPerApplied = &v
	}
	return r, nil
}

// loadFacts is the shared fetch of Load and LoadRetroPack: the window's rows
// and the previous equal window's, both filtered, both oldest first.
func loadFacts(ctx context.Context, st *store.Store, q Query) (current, prev []store.AttemptFacts, err error) {
	window := q.Until.Sub(q.Since)
	if window <= 0 {
		return nil, nil, fmt.Errorf("stats window: until %s is not after since %s", q.Until.Format(time.RFC3339), q.Since.Format(time.RFC3339))
	}
	if current, err = st.FactsSince(ctx, q.Since, q.Until, q.Routine); err != nil {
		return nil, nil, fmt.Errorf("load stats window: %w", err)
	}
	if prev, err = st.FactsSince(ctx, q.Since.Add(-window), q.Since, q.Routine); err != nil {
		return nil, nil, fmt.Errorf("load previous stats window: %w", err)
	}
	return filter(current, q), filter(prev, q), nil
}

// filter applies the dimensions the facts query cannot: repository, project,
// and mode live on every row, so Go filters the fetched range.
func filter(rows []store.AttemptFacts, q Query) []store.AttemptFacts {
	if q.Repository == "" && q.Project == "" && q.Mode == "" && q.Size == "" {
		return rows
	}
	out := make([]store.AttemptFacts, 0, len(rows))
	for _, f := range rows {
		if q.Repository != "" && f.Repository != q.Repository {
			continue
		}
		if q.Project != "" && f.Project != q.Project {
			continue
		}
		if q.Mode != "" && f.Mode != q.Mode {
			continue
		}
		if q.Size != "" && f.Size != q.Size && !(q.Size == "unsized" && f.Size == "") {
			continue
		}
		out = append(out, f)
	}
	return out
}

// aggregate groups rows by routine (byGeneration false) or by (routine,
// generation) and finishes each group's aggregate, sorted for stable output.
func aggregate(rows []store.AttemptFacts, byGeneration bool) []RoutineStats {
	groups := map[string]*accumulator{}
	for i := range rows {
		f := &rows[i]
		gen := 0
		if byGeneration {
			gen = f.Generation
		}
		k := Key(f.Routine, gen)
		acc := groups[k]
		if acc == nil {
			acc = newAccumulator(f.Routine, gen)
			groups[k] = acc
		}
		acc.add(f)
	}
	out := make([]RoutineStats, 0, len(groups))
	for _, acc := range groups {
		out = append(out, acc.finish())
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Routine != out[j].Routine {
			return out[i].Routine < out[j].Routine
		}
		return out[i].Generation < out[j].Generation
	})
	return out
}

// accumulator carries one group's running sums and the per-column denominators
// the doc comment on RoutineStats promises (NULL columns never count).
type accumulator struct {
	s                           RoutineStats
	totals, agents              []int64
	tokensIn, tokensOut         int64
	tokensInRuns, tokensOutRuns int
	costRuns                    int
	selfOK, selfReported        int
	turns, turnRuns             int
	questions, questionRuns     int
	reasons                     map[string]int
}

func newAccumulator(routine string, generation int) *accumulator {
	return &accumulator{
		s:       RoutineStats{Routine: routine, Generation: generation, Outcomes: map[string]int{}, ToolMix: map[string]int{}},
		reasons: map[string]int{},
	}
}

func (a *accumulator) add(f *store.AttemptFacts) {
	a.s.Runs++
	a.s.Outcomes[string(f.State)]++
	if model.IsSuccess(f.State) && f.VerificationPass != nil && *f.VerificationPass {
		a.s.VerifiedSuccesses++
	}
	if f.IsError != nil {
		a.selfReported++
		if !*f.IsError {
			a.selfOK++
		}
	}
	if v := f.Phases["total"]; v != nil {
		a.totals = append(a.totals, *v)
	}
	if v := f.Phases["agent"]; v != nil {
		a.agents = append(a.agents, *v)
	}
	if f.InputTokens != nil {
		a.tokensIn += *f.InputTokens
		a.tokensInRuns++
	}
	if f.OutputTokens != nil {
		a.tokensOut += *f.OutputTokens
		a.tokensOutRuns++
	}
	if f.CostUSD != nil {
		a.s.CostUSDTotal += *f.CostUSD
		a.costRuns++
	}
	if f.Turns != nil {
		a.turns += *f.Turns
		a.turnRuns++
	}
	if f.QuestionsAsked != nil {
		a.questions += *f.QuestionsAsked
		a.questionRuns++
	}
	for name, n := range f.ToolCallsByName {
		a.s.ToolMix[name] += n
	}
	if f.FailureReason != "" {
		a.reasons[string(f.FailureReason)]++
	}
	if f.UtilizationDelta != nil && *f.UtilizationDelta > 0 {
		a.s.UtilizationConsumed += *f.UtilizationDelta
	}
}

func (a *accumulator) finish() RoutineStats {
	s := a.s
	s.VerifiedSuccessRate = ratio(s.VerifiedSuccesses, s.Runs)
	s.SelfReportedSuccessRate = ratio(a.selfOK, a.selfReported)
	s.P50TotalUS, s.P95TotalUS, s.MaxTotalUS = percentiles(a.totals)
	s.P50AgentUS, s.P95AgentUS, s.MaxAgentUS = percentiles(a.agents)
	s.TokensInPerRun = ratio64(a.tokensIn, a.tokensInRuns)
	s.TokensOutPerRun = ratio64(a.tokensOut, a.tokensOutRuns)
	s.CostPerRun = 0
	if a.costRuns > 0 {
		s.CostPerRun = s.CostUSDTotal / float64(a.costRuns)
	}
	if s.VerifiedSuccesses > 0 {
		v := s.CostUSDTotal / float64(s.VerifiedSuccesses)
		s.CostPerVerifiedSuccess = &v
	}
	s.TurnsPerRun = ratio(a.turns, a.turnRuns)
	s.QuestionsPerRun = ratio(a.questions, a.questionRuns)
	s.TopFailureReasons = topReasons(a.reasons, 5)
	return s
}

func ratio(numerator, denominator int) float64 {
	if denominator == 0 {
		return 0
	}
	return float64(numerator) / float64(denominator)
}

func ratio64(numerator int64, denominator int) float64 {
	if denominator == 0 {
		return 0
	}
	return float64(numerator) / float64(denominator)
}

// percentiles sorts values and returns their nearest-rank p50, p95, and max;
// all zero for an empty slice (no run carried the phase).
func percentiles(values []int64) (p50, p95, maxV int64) {
	if len(values) == 0 {
		return 0, 0, 0
	}
	sort.Slice(values, func(i, j int) bool { return values[i] < values[j] })
	return nearestRank(values, 0.50), nearestRank(values, 0.95), values[len(values)-1]
}

// nearestRank is the ⌈p·n⌉-th smallest value of a sorted, non-empty slice —
// the percentile definition STYLE.md §4 mandates computing in Go.
func nearestRank(sorted []int64, p float64) int64 {
	idx := int(math.Ceil(p*float64(len(sorted)))) - 1
	if idx < 0 {
		idx = 0
	}
	return sorted[idx]
}

// topReasons orders failure reasons by count desc (ties alphabetically, so
// output is stable) and caps the list.
func topReasons(counts map[string]int, cap int) []ReasonCount {
	out := make([]ReasonCount, 0, len(counts))
	for reason, n := range counts {
		out = append(out, ReasonCount{Reason: reason, Count: n})
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Count != out[j].Count {
			return out[i].Count > out[j].Count
		}
		return out[i].Reason < out[j].Reason
	})
	if len(out) > cap {
		out = out[:cap]
	}
	return out
}

// RetroRoutine is one routine's current prompt and settings — what the retro
// reader compares outcomes against.
type RetroRoutine struct {
	Name           string            `json:"name"`
	Generation     int               `json:"generation"`
	Mode           string            `json:"mode"`
	Model          string            `json:"model"`
	Prompt         string            `json:"prompt"`
	Effort         string            `json:"effort,omitempty"`
	MaxTurns       int               `json:"max_turns,omitempty"`
	TimeoutSeconds int               `json:"timeout_seconds"`
	Autonomy       model.Autonomy    `json:"autonomy,omitempty"`
	BudgetClass    model.BudgetClass `json:"budget_class"`
	Schedule       string            `json:"schedule,omitempty"`
	AllowedTools   []string          `json:"allowed_tools,omitempty"`
}

// ProblemAttempt is one in-window facts row the retro reads closely, with the
// attempt's structured result, its free-text result bounded to resultTextCap
// bytes, and the last eventTail non-line events of its timeline.
type ProblemAttempt struct {
	Facts      store.AttemptFacts  `json:"facts"`
	Result     json.RawMessage     `json:"result,omitempty"`
	ResultText string              `json:"result_text,omitempty"`
	Events     []store.StoredEvent `json:"events"`
}

// RetroPack is `forge retro`'s product (DESIGN.md §9.4): the stats with their
// previous-window deltas, the current prompt and settings per routine, and
// the problem attempts, newest first.
type RetroPack struct {
	SchemaVersion   int              `json:"schema_version"`
	Window          Query            `json:"window"`
	Stats           *Report          `json:"stats"`
	Routines        []RetroRoutine   `json:"routines"`
	ProblemAttempts []ProblemAttempt `json:"problem_attempts"`
	// Assessments are the window's supervise verdicts — outcome, scores, and
	// the weakness prose, resolved to the ask they judged. Per-directive
	// stats say WHICH prompt underperforms; these say WHAT the finished
	// products were missing — the reflection evidence for systemic failures
	// that no single attempt exhibits (an overwrite between parallel tasks,
	// docs drifting from code).
	Assessments []store.SuperviseAssessment `json:"supervise_assessments,omitempty"`
}

// problem selects the rows worth a close read: anything that was not a clean
// verified success — a non-succeeded state, a recorded verification failure,
// or a retained worktree kept for inspection.
func problem(f *store.AttemptFacts) bool {
	return f.State != model.Succeeded || (f.VerificationPass != nil && !*f.VerificationPass) || f.Retained
}

// LoadRetroPack builds the retro data pack for one window.
func LoadRetroPack(ctx context.Context, st *store.Store, q Query) (*RetroPack, error) {
	current, prev, err := loadFacts(ctx, st, q)
	if err != nil {
		return nil, err
	}
	report := Compute(current, prev)
	report.Query = q
	routines, err := st.ListRoutines(ctx, false)
	if err != nil {
		return nil, fmt.Errorf("load routines for retro: %w", err)
	}
	rr := make([]RetroRoutine, 0, len(routines))
	for _, rt := range routines {
		rr = append(rr, RetroRoutine{
			Name: rt.Name, Generation: rt.Generation, Mode: rt.Mode, Model: rt.Model, Prompt: rt.Prompt,
			Effort: rt.Effort, MaxTurns: rt.MaxTurns, TimeoutSeconds: rt.TimeoutSeconds, Autonomy: rt.Autonomy,
			BudgetClass: rt.BudgetClass, Schedule: rt.Schedule, AllowedTools: rt.AllowedTools,
		})
	}
	problems := []ProblemAttempt{}
	// current is oldest first; walk backwards so the cap keeps the newest.
	for i := len(current) - 1; i >= 0 && len(problems) < problemCap; i-- {
		f := current[i]
		if !problem(&f) {
			continue
		}
		a, err := st.GetAttempt(ctx, f.AttemptID)
		if err != nil {
			return nil, fmt.Errorf("load problem attempt %s: %w", f.AttemptID, err)
		}
		// Fetch every non-line event and keep the tail: the design wants the
		// LAST 30, and the store's limit takes the head.
		events, err := st.Events(ctx, f.AttemptID, false, 0)
		if err != nil {
			return nil, fmt.Errorf("load events of %s: %w", f.AttemptID, err)
		}
		if len(events) > eventTail {
			events = events[len(events)-eventTail:]
		}
		if events == nil {
			events = []store.StoredEvent{}
		}
		text := a.ResultText
		if len(text) > resultTextCap {
			text = text[:resultTextCap]
		}
		problems = append(problems, ProblemAttempt{Facts: f, Result: a.Result, ResultText: text, Events: events})
	}
	assessments, err := st.RecentAssessments(ctx, q.Since, 12)
	if err != nil {
		return nil, fmt.Errorf("load assessments for retro: %w", err)
	}
	return &RetroPack{SchemaVersion: SchemaVersion, Window: q, Stats: report, Routines: rr, ProblemAttempts: problems, Assessments: assessments}, nil
}
