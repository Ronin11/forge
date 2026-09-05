package tools

// The fact tools: read-only views over usage samples, attempts, events,
// prompt versions, the queue, stats, and the retro data pack.

import (
	"context"
	"encoding/json"
	"errors"
	"sort"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/stats"
	"forge/internal/core/store"
)

// windowNames are the two subscription windows samples are recorded for.
var windowNames = []string{"five_hour", "seven_day"}

type usageTool struct{}

func (usageTool) Name() string { return "forge_usage" }
func (usageTool) Description() string {
	return "Latest subscription rate-limit utilization per window (five_hour, seven_day); a window with no sample yet is null."
}
func (usageTool) Where() string                { return WhereDaemon }
func (usageTool) InputSchema() json.RawMessage { return json.RawMessage(emptySchema) }

// usageWindow is one window's latest observation.
type usageWindow struct {
	Utilization float64   `json:"utilization"`
	ResetsAt    time.Time `json:"resets_at"`
	SampledAt   time.Time `json:"sampled_at"`
}

func (usageTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	if err := decodeInput(req.Input, &struct{}{}); err != nil {
		return nil, err
	}
	windows := map[string]*usageWindow{}
	for _, w := range windowNames {
		sm, err := req.Deps.Store.LatestSample(ctx, w)
		if err != nil {
			return nil, err
		}
		if sm == nil {
			windows[w] = nil
			continue
		}
		windows[w] = &usageWindow{Utilization: sm.Utilization, ResetsAt: sm.ResetsAt, SampledAt: sm.Time}
	}
	return respond(map[string]any{
		"schema_version": SchemaVersion,
		"windows":        windows,
		"note":           "targets and forecasting arrive in M3",
	})
}

type attemptTool struct{}

func (attemptTool) Name() string { return "forge_attempt" }
func (attemptTool) Description() string {
	return "The calling attempt (or any attempt by id): the attempt row, its target state, and its facts once terminal (null before)."
}
func (attemptTool) Where() string { return WhereDaemon }
func (attemptTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{"attempt_id":{"type":"string","description":"defaults to the calling attempt"}},"additionalProperties":false}`)
}

func (attemptTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	var in struct {
		AttemptID string `json:"attempt_id"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	id, err := attemptID(in.AttemptID, req)
	if err != nil {
		return nil, err
	}
	a, err := req.Deps.Store.GetAttempt(ctx, id)
	if err != nil {
		return nil, err
	}
	t, err := req.Deps.Store.GetTarget(ctx, a.TargetID)
	if err != nil {
		return nil, err
	}
	facts, err := req.Deps.Store.FactsForAttempt(ctx, id)
	if err != nil && !errors.Is(err, store.ErrNotFound) {
		return nil, err
	}
	return respond(map[string]any{
		"schema_version": SchemaVersion,
		"attempt":        a,
		"target":         t,
		"facts":          facts, // nil until the attempt is terminal
	})
}

// attemptID resolves an optional attempt_id input against the caller.
func attemptID(in string, req Request) (string, error) {
	if in == "" {
		in = req.AttemptID
	}
	if err := model.ValidateID(in); err != nil {
		return "", BadInput("attempt_id: %v", err)
	}
	return in, nil
}

type eventsTool struct{}

func (eventsTool) Name() string { return "forge_events" }
func (eventsTool) Description() string {
	return "An attempt's event timeline (spans, metrics, lifecycle), ordered by elapsed time; include_lines adds stdout/stderr lines."
}
func (eventsTool) Where() string { return WhereDaemon }
func (eventsTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{"attempt_id":{"type":"string","description":"defaults to the calling attempt"},"include_lines":{"type":"boolean"},` + pageProps + `},"additionalProperties":false}`)
}

func (eventsTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	var in struct {
		AttemptID    string `json:"attempt_id"`
		IncludeLines bool   `json:"include_lines"`
		page
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if err := in.clamp(); err != nil {
		return nil, err
	}
	id, err := attemptID(in.AttemptID, req)
	if err != nil {
		return nil, err
	}
	// An unknown attempt is 404, not an empty list.
	if _, err := req.Deps.Store.GetAttempt(ctx, id); err != nil {
		return nil, err
	}
	events, err := req.Deps.Store.Events(ctx, id, in.IncludeLines, in.Limit+in.Offset)
	if err != nil {
		return nil, err
	}
	events = slicePage(events, in.page)
	return respond(map[string]any{
		"schema_version": SchemaVersion,
		"attempt_id":     id,
		"events":         events,
		"count":          len(events),
	})
}

type promptVersionTool struct{}

func (promptVersionTool) Name() string { return "forge_prompt_version" }
func (promptVersionTool) Description() string {
	return "The exact prompt configuration an attempt ran with, by hash; defaults to the calling attempt's."
}
func (promptVersionTool) Where() string { return WhereDaemon }
func (promptVersionTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{"hash":{"type":"string","description":"defaults to the calling attempt's prompt_version_hash"}},"additionalProperties":false}`)
}

func (promptVersionTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	var in struct {
		Hash string `json:"hash"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if in.Hash == "" {
		a, err := req.Deps.Store.GetAttempt(ctx, req.AttemptID)
		if err != nil {
			return nil, err
		}
		if a.PromptVersionHash == "" {
			return nil, BadInput("attempt %s has no prompt version recorded yet", req.AttemptID)
		}
		in.Hash = a.PromptVersionHash
	}
	pv, err := req.Deps.Store.GetPromptVersion(ctx, in.Hash)
	if err != nil {
		return nil, err
	}
	return respond(map[string]any{"schema_version": SchemaVersion, "prompt_version": pv})
}

type queueTool struct{}

func (queueTool) Name() string { return "forge_queue" }
func (queueTool) Description() string {
	return "The open Work queue in claim order: priority desc, budget class, then age, with each Work's derived state."
}
func (queueTool) Where() string { return WhereDaemon }
func (queueTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{` + pageProps + `},"additionalProperties":false}`)
}

// queueRow is one Work in the queue view, numbers pre-computed.
type queueRow struct {
	WorkID    string            `json:"work_id"`
	Routine   string            `json:"routine"`
	Title     string            `json:"title"`
	State     model.WorkState   `json:"state"`
	Priority  int               `json:"priority"`
	Class     model.BudgetClass `json:"class"`
	CreatedAt time.Time         `json:"created_at"`
}

func (queueTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	var in page
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if err := in.clamp(); err != nil {
		return nil, err
	}
	work, err := req.Deps.Store.OpenWork(ctx)
	if err != nil {
		return nil, err
	}
	ids := make([]string, len(work))
	for i, w := range work {
		ids[i] = w.ID
	}
	targets, err := req.Deps.Store.TargetsForWorks(ctx, ids)
	if err != nil {
		return nil, err
	}
	edges, err := req.Deps.Store.DependencyEdges(ctx)
	if err != nil {
		return nil, err
	}
	// The states-then-dependencies pass and the sort key duplicate
	// controlplane/queue.go's Order — the one true queue order — which cannot
	// be imported here (controlplane imports tools). No budget deferral in M2.
	states := map[string]model.WorkState{}
	for _, w := range work {
		states[w.ID] = model.DeriveWorkState(model.WorkInputs{Targets: targetStates(targets[w.ID]), Integrate: w.Integrate})
	}
	edgesByWork := map[string][]model.Edge{}
	for _, e := range edges {
		edgesByWork[e.Work] = append(edgesByWork[e.Work], e)
	}
	rows := make([]queueRow, 0, len(work))
	for _, w := range work {
		deps := model.Dependencies(edgesByWork[w.ID], states)
		st := model.DeriveWorkState(model.WorkInputs{Targets: targetStates(targets[w.ID]), Integrate: w.Integrate, Blocked: !deps.Satisfied})
		rows = append(rows, queueRow{WorkID: w.ID, Routine: w.RoutineName, Title: w.Title, State: st, Priority: w.Priority, Class: w.BudgetClass, CreatedAt: w.CreatedAt})
	}
	sort.SliceStable(rows, func(i, j int) bool {
		a, b := rows[i], rows[j]
		if a.Priority != b.Priority {
			return a.Priority > b.Priority
		}
		if a.Class.Rank() != b.Class.Rank() {
			return a.Class.Rank() < b.Class.Rank()
		}
		return a.CreatedAt.Before(b.CreatedAt)
	})
	rows = slicePage(rows, in)
	return respond(map[string]any{"schema_version": SchemaVersion, "queue": rows, "count": len(rows)})
}

func targetStates(ts []store.Target) []model.State {
	out := make([]model.State, len(ts))
	for i, t := range ts {
		out[i] = t.State
	}
	return out
}

// defaultSinceHours is one week — the stats and retro window default.
const defaultSinceHours = 168

type statsTool struct{}

func (statsTool) Name() string { return "forge_stats" }
func (statsTool) Description() string {
	return "Per-routine and per-generation aggregates over the facts of a window: runs, outcomes, verified vs self-reported success, duration percentiles, tokens, cost, tool mix, failure reasons, and the previous equal window for deltas."
}
func (statsTool) Where() string { return WhereDaemon }
func (statsTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{"since_hours":{"type":"integer","minimum":1,"description":"window size; default 168 (one week)"},"routine":{"type":"string","description":"limit to one routine"}},"additionalProperties":false}`)
}

func (statsTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	var in struct {
		SinceHours int    `json:"since_hours"`
		Routine    string `json:"routine"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	hours, err := sinceHours(in.SinceHours)
	if err != nil {
		return nil, err
	}
	now := req.Deps.Clock().UTC()
	report, err := stats.Load(ctx, req.Deps.Store, stats.Query{Since: now.Add(-time.Duration(hours) * time.Hour), Until: now, Routine: in.Routine})
	if err != nil {
		return nil, err
	}
	return respond(map[string]any{
		"schema_version": SchemaVersion,
		"since_hours":    hours,
		"report":         report,
	})
}

func sinceHours(in int) (int, error) {
	if in < 0 {
		return 0, BadInput("since_hours must be positive")
	}
	if in == 0 {
		return defaultSinceHours, nil
	}
	return in, nil
}

type retroPackTool struct{}

func (retroPackTool) Name() string { return "forge_retro_pack" }
func (retroPackTool) Description() string {
	return "The retro data pack: all-routine stats with previous-window deltas, the current prompt and settings per routine, the newest problem attempts (not verified successes) with their structured result and last span events, and the window's supervise assessments — scores and weakness prose per finished ask, the evidence for systemic failures no single attempt shows."
}
func (retroPackTool) Where() string { return WhereDaemon }
func (retroPackTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{"since_hours":{"type":"integer","minimum":1,"description":"window size; default 168 (one week)"}},"additionalProperties":false}`)
}

func (retroPackTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	var in struct {
		SinceHours int `json:"since_hours"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	hours, err := sinceHours(in.SinceHours)
	if err != nil {
		return nil, err
	}
	now := req.Deps.Clock().UTC()
	pack, err := stats.LoadRetroPack(ctx, req.Deps.Store, stats.Query{Since: now.Add(-time.Duration(hours) * time.Hour), Until: now})
	if err != nil {
		return nil, err
	}
	// The pack already carries schema_version (stats.SchemaVersion): the
	// response is the pack itself, the same shape GET /api/v1/retro serves.
	return respond(pack)
}
