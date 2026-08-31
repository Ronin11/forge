// Package controlplane is the daemon: HTTP API on two listeners, scheduler,
// budget policy, queue, sweeper, facts, bootstrap, and the UI. It is the only
// package that imports the store.
package controlplane

import (
	"encoding/json"
	"sort"
	"time"

	"forge/internal/protocol"
	"forge/internal/store"
)

// FactsInput is everything ComputeFacts looks at; it is assembled by the caller
// from the store so the computation itself is pure and testable.
type FactsInput struct {
	Attempt   store.Attempt
	Target    store.Target
	Work      store.Work
	Project   string
	Events    []store.StoredEvent // every event of the attempt, all sources
	Questions []store.Question
	Samples   []store.RateLimitSample // five_hour samples around the attempt
	Now       time.Time
	// LeaseBlockedAt is when the Target was first passed over for a path
	// lease (journal target.lease_blocked); zero when it never was.
	LeaseBlockedAt time.Time
	// Model is the chosen model's resolved routing info (M10, DESIGN.md §21):
	// its class and price drive the notional usd term. Zero (empty ID) when the
	// alias resolved to a bare id with no routing metadata.
	Model ModelInfo
}

// ComputeFacts is the one function that turns an attempt into its facts row
// (DESIGN.md §9.2). Anything it cannot compute stays nil, never zero.
func ComputeFacts(in FactsInput) *store.AttemptFacts {
	a, t, w := in.Attempt, in.Target, in.Work
	f := &store.AttemptFacts{
		AttemptID: a.ID, TargetID: t.ID, WorkID: w.ID, Routine: w.RoutineName, Generation: w.Generation,
		Project: in.Project, Repository: t.Repository, Worker: a.WorkerID, Executor: a.Executor, Model: a.ModelAlias,
		Effort: a.Effort, Mode: a.Mode, Trigger: w.Trigger, PromptVersionHash: a.PromptVersionHash, Autonomy: a.Autonomy,
		Phases: map[string]*int64{}, StartedAt: a.StartedAt, FinishedAt: finishedAt(a, t, in.Now),
		State: t.State, ExitCode: a.ExitCode, FailureReason: t.FailureReason, VerificationLevel: a.VerificationLevel,
		VerificationPass: a.VerificationPass, Retained: t.Retained, Branch: a.Branch, Base: a.BaseCommit, Head: a.HeadCommit,
	}
	if t.Retained {
		f.RetainedReason = a.Cleanup.Reason
	}
	if !a.FinishedAt.IsZero() || a.ExitCode != nil {
		f.IsError = ptr(a.IsError)
		f.Turns = ptr(a.NumTurns)
		f.InputTokens, f.OutputTokens = ptr(a.Usage.InputTokens), ptr(a.Usage.OutputTokens)
		f.CacheReadTokens, f.CacheCreation = ptr(a.Usage.CacheReadTokens), ptr(a.Usage.CacheCreationTokens)
		f.CostUSD = a.CostUSD
	}
	if a.HeadCommit != "" {
		f.Commits, f.FilesChanged, f.Insertions, f.Deletions = ptr(a.Git.Commits), ptr(a.Git.FilesChanged), ptr(a.Git.Insertions), ptr(a.Git.Deletions)
		f.Dirty, f.Pushed = ptr(a.Git.Dirty), ptr(a.Git.Pushed)
	}
	f.Runner, f.EscalatedFrom, f.ModelClass = a.Runner, a.EscalatedFrom, in.Model.Class
	computePhases(f, in.Events)
	computeTools(f, in.Events)
	computeTokensToFirstEdit(f, in.Events)
	computeEventCounts(f, in.Events)
	computeQuestions(f, in.Questions, in.Now)
	computeBudget(f, a, in.Samples)
	computeCostVector(f, a, in.Model)
	computeWriteSet(f, w, a)
	if !in.LeaseBlockedAt.IsZero() && !t.ClaimedAt.IsZero() && !t.ClaimedAt.Before(in.LeaseBlockedAt) {
		// Wall clock: the block and the claim happen in different
		// transactions (attrs.clock = "wall", DESIGN.md §9.2).
		f.LeaseWaitUS = ptr(t.ClaimedAt.Sub(in.LeaseBlockedAt).Microseconds())
	}
	return f
}

// computeWriteSet fills declared_paths, touched_paths, and
// write_set_precision (M9). Touched paths come from the result envelope's
// changes[] — a claim, but one L0 checked against git when verification
// passed — so an attempt without a parsed result honestly stays NULL.
// Precision is |touched ∩ declared| / |touched| with the scheduler's own
// matcher; a Work with no declared paths leases the whole repository, so
// declared and precision stay NULL rather than pretending 1.0.
func computeWriteSet(f *store.AttemptFacts, w store.Work, a store.Attempt) {
	f.DeclaredPaths = w.Paths
	if len(a.Result) == 0 {
		return
	}
	var env struct {
		Changes []struct {
			Path string `json:"path"`
		} `json:"changes"`
	}
	if json.Unmarshal(a.Result, &env) != nil || env.Changes == nil {
		return
	}
	touched := make([]string, 0, len(env.Changes))
	for _, c := range env.Changes {
		if c.Path != "" {
			touched = append(touched, c.Path)
		}
	}
	sort.Strings(touched)
	f.TouchedPaths = touched
	if len(w.Paths) == 0 || len(touched) == 0 {
		return
	}
	matched := 0
	for _, p := range touched {
		for _, g := range w.Paths {
			if PathMatchesGlob(g, p) {
				matched++
				break
			}
		}
	}
	f.WriteSetPrecision = ptr(float64(matched) / float64(len(touched)))
}

func finishedAt(a store.Attempt, t store.Target, now time.Time) time.Time {
	switch {
	case !a.FinishedAt.IsZero():
		return a.FinishedAt
	case !t.FinishedAt.IsZero():
		return t.FinishedAt
	}
	return now
}

func ptr[T any](v T) *T { return &v }

// computePhases sums each phase span's duration_us over launches; total is the
// sum of all phases (so it excludes human waiting by construction).
func computePhases(f *store.AttemptFacts, events []store.StoredEvent) {
	var total int64
	seen := false
	for _, e := range events {
		if e.Kind != protocol.KindSpanEnd || e.ParentID != "" && !isPhase(e.Name) {
			continue
		}
		name := e.Name
		if len(name) > 6 && name[:6] == "agent-" {
			name = "agent"
		}
		if !isPhase(name) {
			continue
		}
		v := f.Phases[name]
		if v == nil {
			v = new(int64)
			f.Phases[name] = v
		}
		*v += e.DurationUS
		if name != "queue_wait" {
			total += e.DurationUS
			seen = true
		}
	}
	if seen {
		f.Phases["total"] = &total
	}
}

func isPhase(name string) bool {
	for _, p := range store.PhaseNames {
		if p == name && p != "total" {
			return true
		}
	}
	return name == "agent"
}

// computeTools derives tool counts and durations from tool spans (children of
// an agent span) and mcp spans. Duration is span_end.elapsed − span_start.elapsed
// when the parser left duration_us zero.
func computeTools(f *store.AttemptFacts, events []store.StoredEvent) {
	starts := map[string]store.StoredEvent{}
	byName, timeByName := map[string]int{}, map[string]int64{}
	var durations []int64
	errorsN, total := 0, 0
	for _, e := range events {
		isTool := e.Kind == protocol.KindSpanStart && (hasPrefix(e.ParentID, "agent-") || e.Source == protocol.SourceMCP)
		if isTool {
			starts[e.Source+"/"+e.SpanID] = e
			byName[e.Name]++
			total++
			continue
		}
		if e.Kind != protocol.KindSpanEnd {
			continue
		}
		start, ok := starts[e.Source+"/"+e.SpanID]
		if !ok {
			continue
		}
		d := e.DurationUS
		if d == 0 && e.ElapsedUS >= start.ElapsedUS {
			d = e.ElapsedUS - start.ElapsedUS
		}
		timeByName[e.Name] += d
		durations = append(durations, d)
		var attrs struct {
			IsError bool `json:"is_error"`
		}
		if len(e.Attrs) > 0 && json.Unmarshal(e.Attrs, &attrs) == nil && attrs.IsError {
			errorsN++
		}
	}
	if total == 0 {
		f.ToolCallsByName, f.ToolTimeByName = map[string]int{}, map[string]int64{}
		return
	}
	f.ToolCallsTotal, f.ToolErrors = &total, &errorsN
	f.ToolCallsByName, f.ToolTimeByName = byName, timeByName
	if len(durations) > 0 {
		sort.Slice(durations, func(i, j int) bool { return durations[i] < durations[j] })
		f.ToolP50US = ptr(durations[len(durations)/2])
		f.ToolMaxUS = ptr(durations[len(durations)-1])
	}
}

func hasPrefix(s, p string) bool { return len(s) >= len(p) && s[:len(p)] == p }

// editToolNames are the built-in tools that mutate files; the first span with
// one of these names marks "first edit" for tokens_to_first_edit. Bash is
// excluded on purpose: it may or may not mutate and there is no cheap signal.
var editToolNames = map[string]bool{"Edit": true, "Write": true, "MultiEdit": true, "NotebookEdit": true}

// computeTokensToFirstEdit sums the per-message usage metrics (input + output
// tokens) recorded up to and including the assistant message that makes the
// first file-mutating tool call (M11 repo briefs, DESIGN §22). Events are
// ordered by elapsed time and a message's usage metric is emitted before its
// tool spans, so the editing message's own tokens count. NULL when the
// attempt never edited.
func computeTokensToFirstEdit(f *store.AttemptFacts, events []store.StoredEvent) {
	var total int64
	for _, e := range events {
		if e.Kind == protocol.KindMetric && e.Name == "usage" {
			var attrs struct {
				Input  int64 `json:"input_tokens"`
				Output int64 `json:"output_tokens"`
			}
			if json.Unmarshal(e.Attrs, &attrs) == nil {
				total += attrs.Input + attrs.Output
			}
			continue
		}
		if e.Kind == protocol.KindSpanStart && hasPrefix(e.ParentID, "agent-") && editToolNames[e.Name] {
			f.TokensToFirstEdit = &total
			return
		}
	}
}

func computeEventCounts(f *store.AttemptFacts, events []store.StoredEvent) {
	n := len(events)
	dropped := 0
	for _, e := range events {
		if e.Kind == protocol.KindLifecycle && e.Name == "events_dropped" {
			var attrs struct {
				Dropped int `json:"dropped"`
			}
			if json.Unmarshal(e.Attrs, &attrs) == nil {
				dropped += attrs.Dropped
			}
		}
	}
	f.EventsTotal, f.EventsDropped = &n, &dropped
}

// computeQuestions counts questions and sums their open intervals (wall clock —
// the one such column, see DESIGN.md §9.2).
func computeQuestions(f *store.AttemptFacts, qs []store.Question, now time.Time) {
	n := len(qs)
	f.QuestionsAsked = &n
	var wait time.Duration
	for _, q := range qs {
		end := q.AnsweredAt
		if end.IsZero() {
			end = now
		}
		wait += end.Sub(q.AskedAt)
	}
	f.WaitHumanUS = ptr(wait.Microseconds())
}

// computeBudget takes the last sample before the attempt started and the last
// sample this attempt produced; the delta is an estimate only when both came
// from this attempt's own window of samples.
func computeBudget(f *store.AttemptFacts, a store.Attempt, samples []store.RateLimitSample) {
	if a.StartedAt.IsZero() {
		return
	}
	var before, after *store.RateLimitSample
	for i := range samples {
		s := samples[i]
		if s.Window != "five_hour" {
			continue
		}
		if !s.Time.After(a.StartedAt) {
			before = &samples[i]
		}
		if s.SourceAttempt == a.ID {
			after = &samples[i]
		}
	}
	if before != nil {
		f.FiveHourBefore = ptr(before.Utilization)
	}
	if after != nil {
		f.FiveHourAfter = ptr(after.Utilization)
	}
	if before != nil && after != nil && after.ResetsAt.Equal(before.ResetsAt) {
		f.UtilizationDelta = ptr(after.Utilization - before.Utilization)
	}
	var sdBefore, sdAfter *store.RateLimitSample
	for i := range samples {
		s := samples[i]
		if s.Window != "seven_day" {
			continue
		}
		if !s.Time.After(a.StartedAt) {
			sdBefore = &samples[i]
		}
		if s.SourceAttempt == a.ID {
			sdAfter = &samples[i]
		}
	}
	if sdBefore != nil {
		f.SevenDayBefore = ptr(sdBefore.Utilization)
	}
	if sdAfter != nil {
		f.SevenDayAfter = ptr(sdAfter.Utilization)
	}
	// The window deltas the router scores (M10, DESIGN.md §21): the movement
	// bracketing the attempt, only when both samples came from the same
	// unreset window. NULL otherwise — honest, never a fabricated zero.
	if before != nil && after != nil && after.ResetsAt.Equal(before.ResetsAt) {
		f.FiveHourDelta = ptr(after.Utilization - before.Utilization)
	}
	if sdBefore != nil && sdAfter != nil && sdAfter.ResetsAt.Equal(sdBefore.ResetsAt) {
		f.SevenDayDelta = ptr(sdAfter.Utilization - sdBefore.Utilization)
	}
}

// computeCostVector fills the M10 usd and runner_seconds terms. usd is
// notional (tokens × the model's list price, $/MTok); it is computed whenever
// the model carries a price and the attempt reported tokens, on subscription or
// api alike (the subscription figure is notional, the billing tells the reader
// which). runner_seconds is the agent phase's wall seconds.
func computeCostVector(f *store.AttemptFacts, a store.Attempt, info ModelInfo) {
	p := info.Price
	if (p.Input != 0 || p.Output != 0 || p.CacheRead != 0 || p.CacheWrite != 0) && (a.Usage.InputTokens != 0 || a.Usage.OutputTokens != 0 || a.Usage.CacheReadTokens != 0 || a.Usage.CacheCreationTokens != 0) {
		usd := (float64(a.Usage.InputTokens)*p.Input +
			float64(a.Usage.OutputTokens)*p.Output +
			float64(a.Usage.CacheReadTokens)*p.CacheRead +
			float64(a.Usage.CacheCreationTokens)*p.CacheWrite) / 1e6
		f.USD = ptr(usd)
	}
	if agent := f.Phases["agent"]; agent != nil {
		f.RunnerSeconds = ptr(float64(*agent) / 1e6)
	}
}
