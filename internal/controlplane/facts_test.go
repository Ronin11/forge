package controlplane

import (
	"encoding/json"
	"testing"
	"time"

	"forge/internal/model"
	"forge/internal/protocol"
	"forge/internal/store"
)

func ev(source, kind, name, span, parent string, elapsed, dur int64, attrs string) store.StoredEvent {
	e := store.StoredEvent{Source: source, Event: protocol.Event{Kind: kind, Name: name, SpanID: span, ParentID: parent, ElapsedUS: elapsed, DurationUS: dur}}
	if attrs != "" {
		e.Attrs = json.RawMessage(attrs)
	}
	return e
}

func TestComputeFacts(t *testing.T) {
	now := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	started := now.Add(-time.Minute)
	exit := 0
	cost := 0.02
	in := FactsInput{
		Attempt: store.Attempt{ID: "a1", WorkerID: "w", Executor: "claude-code", ModelAlias: "haiku", Mode: "run", Autonomy: model.AutonomyAuto, StartedAt: started, FinishedAt: now, ExitCode: &exit, NumTurns: 3,
			Usage: protocol.Usage{InputTokens: 10, OutputTokens: 20}, CostUSD: &cost, HeadCommit: "h", BaseCommit: "b", Branch: "forge/x", Git: protocol.GitOutcome{Commits: 1, Pushed: false, Dirty: false}, Cleanup: protocol.Cleanup{Reason: "unpushed commits"}},
		Target:  store.Target{ID: "t1", Repository: "equitizr", State: model.Succeeded, Retained: true},
		Work:    store.Work{ID: "w1", RoutineName: "inventory", Generation: 2, Trigger: model.TriggerManual},
		Project: "default",
		Events: []store.StoredEvent{
			ev("control", "span_end", "queue_wait", "queue_wait", "", 0, 5_000_000, ""),
			ev("worker", "span_end", "fetch", "fetch", "", 100, 90, ""),
			ev("worker", "span_end", "agent-1", "agent-1", "", 1000, 800, ""),
			ev("worker", "span_start", "Bash", "s1", "agent-1", 200, 0, `{"tool":"Bash"}`),
			ev("worker", "span_end", "Bash", "s1", "agent-1", 260, 0, `{"is_error":false}`),
			ev("worker", "span_start", "Bash", "s2", "agent-1", 300, 0, ""),
			ev("worker", "span_end", "Bash", "s2", "agent-1", 340, 0, `{"is_error":true}`),
			ev("mcp", "span_start", "forge_repo_status", "mcp-1", "agent-1", 400, 0, ""),
			ev("mcp", "span_end", "forge_repo_status", "mcp-1", "agent-1", 500, 70, ""),
			ev("worker", "span_end", "agent-2", "agent-2", "", 2000, 200, ""),
			ev("worker", "span_end", "cleanup", "cleanup", "", 3000, 10, ""),
			{Source: "worker", Event: protocol.Event{Kind: "lifecycle", Name: "events_dropped", Attrs: json.RawMessage(`{"dropped":7}`)}},
			{Source: "worker", Event: protocol.Event{Kind: "stdout", Message: "x"}},
		},
		Questions: []store.Question{{AskedAt: started.Add(10 * time.Second), AnsweredAt: started.Add(20 * time.Second)}, {AskedAt: now.Add(-5 * time.Second)}},
		Samples: []store.RateLimitSample{
			{Time: started.Add(-time.Hour), Window: "five_hour", Utilization: 0.10, ResetsAt: now.Add(time.Hour)},
			{Time: started.Add(-time.Minute), Window: "five_hour", Utilization: 0.20, ResetsAt: now.Add(time.Hour)},
			{Time: now, Window: "five_hour", Utilization: 0.25, ResetsAt: now.Add(time.Hour), SourceAttempt: "a1"},
			{Time: started.Add(-time.Minute), Window: "seven_day", Utilization: 0.5, ResetsAt: now.Add(48 * time.Hour)},
		},
		Now: now,
	}
	f := ComputeFacts(in)
	want := map[string]int64{"queue_wait": 5_000_000, "fetch": 90, "agent": 1000, "cleanup": 10, "total": 1100}
	for name, v := range want {
		if f.Phases[name] == nil || *f.Phases[name] != v {
			t.Errorf("phase %s = %v, want %d", name, f.Phases[name], v)
		}
	}
	if f.Phases["manifest"] != nil {
		t.Error("absent phase must be NULL")
	}
	if *f.ToolCallsTotal != 3 || f.ToolCallsByName["Bash"] != 2 || f.ToolCallsByName["forge_repo_status"] != 1 || *f.ToolErrors != 1 {
		t.Errorf("tools: total=%d byName=%v errors=%d", *f.ToolCallsTotal, f.ToolCallsByName, *f.ToolErrors)
	}
	if f.ToolTimeByName["Bash"] != 100 || f.ToolTimeByName["forge_repo_status"] != 70 || *f.ToolP50US != 60 || *f.ToolMaxUS != 70 {
		t.Errorf("tool time: %v p50=%d max=%d", f.ToolTimeByName, *f.ToolP50US, *f.ToolMaxUS)
	}
	if *f.EventsTotal != 13 || *f.EventsDropped != 7 {
		t.Errorf("events %d dropped %d", *f.EventsTotal, *f.EventsDropped)
	}
	if *f.QuestionsAsked != 2 || *f.WaitHumanUS != (10*time.Second+5*time.Second).Microseconds() {
		t.Errorf("questions %d wait %d", *f.QuestionsAsked, *f.WaitHumanUS)
	}
	if *f.FiveHourBefore != 0.20 || *f.FiveHourAfter != 0.25 || *f.UtilizationDelta-0.05 > 1e-9 || *f.SevenDayBefore != 0.5 || f.SevenDayAfter != nil {
		t.Errorf("budget: %v %v %v %v %v", *f.FiveHourBefore, *f.FiveHourAfter, *f.UtilizationDelta, *f.SevenDayBefore, f.SevenDayAfter)
	}
	if !f.Retained || f.RetainedReason != "unpushed commits" || *f.Commits != 1 || *f.Pushed || *f.Turns != 3 || *f.CostUSD != 0.02 || f.Model != "haiku" || f.Generation != 2 {
		t.Errorf("outcome: %+v", f)
	}
	empty := ComputeFacts(FactsInput{Attempt: store.Attempt{ID: "a2"}, Target: store.Target{State: model.Failed}, Work: store.Work{}, Now: now})
	if empty.Phases["total"] != nil || empty.Turns != nil || empty.ToolCallsTotal != nil || empty.FiveHourBefore != nil || empty.Commits != nil || empty.FinishedAt != now {
		t.Errorf("empty attempt must yield NULLs: %+v", empty)
	}
}
