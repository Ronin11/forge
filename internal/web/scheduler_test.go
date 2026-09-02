package web

import (
	"context"
	"net/http"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// One tick backfills next_due_at for an enabled schedule; the next due tick
// fires a Work stamped trigger=schedule and advances next_due_at; a still-open
// occurrence is skipped, journaled, and rescheduled.
func TestSchedulerFiresRoutine(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	ctx := context.Background()

	rt := store.Routine{Name: "cronny", Mode: "run", Prompt: "p", Repositories: []string{"equitizr"}, Model: "haiku", TimeoutSeconds: 300,
		Schedule: "0 3 * * *", ScheduleEnabled: true}
	h.call(http.MethodPost, "/api/v1/routines", rt, &rt, http.StatusCreated)

	// Tick 1: backfill only — next_due_at gets seeded in the future.
	h.srv.scheduleTick(ctx)
	saved, err := h.st.GetRoutine(ctx, "cronny")
	if err != nil || saved.NextDueAt.IsZero() {
		t.Fatalf("next_due_at not backfilled: %+v, %v", saved, err)
	}
	if n, err := h.st.OpenWorkCountForRoutine(ctx, saved.ID); err != nil || n != 0 {
		t.Fatalf("fired at backfill: %d works, %v", n, err)
	}

	// Cross the occurrence: the tick fires one Work with trigger schedule.
	h.clock.Advance(saved.NextDueAt.Sub(h.clock.Now()) + time.Minute)
	h.srv.scheduleTick(ctx)
	works, err := h.st.OpenWork(ctx)
	if err != nil {
		t.Fatal(err)
	}
	var fired *store.Work
	for i := range works {
		if works[i].RoutineName == "cronny" {
			fired = &works[i]
		}
	}
	if fired == nil || fired.Trigger != model.TriggerSchedule {
		t.Fatalf("fired = %+v", fired)
	}
	after, err := h.st.GetRoutine(ctx, "cronny")
	if err != nil {
		t.Fatal(err)
	}
	if !after.NextDueAt.After(h.clock.Now()) {
		t.Fatalf("next_due_at not advanced: %v vs %v", after.NextDueAt, h.clock.Now())
	}

	// Next occurrence with the first Work still open: skipped, rescheduled.
	h.clock.Advance(after.NextDueAt.Sub(h.clock.Now()) + time.Minute)
	h.srv.scheduleTick(ctx)
	works, err = h.st.OpenWork(ctx)
	if err != nil {
		t.Fatal(err)
	}
	count := 0
	for _, w := range works {
		if w.RoutineName == "cronny" {
			count++
		}
	}
	if count != 1 {
		t.Fatalf("skip-if-running violated: %d open works", count)
	}
}

// A scheduled workflow fires a run with trigger=schedule whose root
// materializes; while it is open, the next occurrence is skipped.
func TestSchedulerFiresWorkflow(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	ctx := context.Background()
	h.createRoutine("step-r")

	wf := store.Workflow{Name: "nightly-flow", Steps: []store.WorkflowStep{{Name: "a", Routine: "step-r"}},
		Schedule: "30 2 * * *", ScheduleEnabled: true}
	h.call(http.MethodPost, "/api/v1/workflows", wf, nil, http.StatusCreated)

	h.srv.scheduleTick(ctx) // backfill
	saved, err := h.st.GetWorkflow(ctx, "nightly-flow")
	if err != nil || saved.NextDueAt.IsZero() {
		t.Fatalf("workflow next_due_at not backfilled: %+v, %v", saved, err)
	}
	h.clock.Advance(saved.NextDueAt.Sub(h.clock.Now()) + time.Minute)
	h.srv.scheduleTick(ctx)

	runs, err := h.st.WorkflowRunsFor(ctx, "nightly-flow", 10)
	if err != nil || len(runs) != 1 {
		t.Fatalf("runs = %+v, %v", runs, err)
	}
	if runs[0].Trigger != model.TriggerSchedule || runs[0].Status != store.RunRunning {
		t.Fatalf("run = %+v", runs[0])
	}
	nodes, err := h.st.RunNodes(ctx, runs[0].ID)
	if err != nil || len(nodes) != 1 || nodes[0].Status != store.NodeRunning || nodes[0].WorkID == "" {
		t.Fatalf("nodes = %+v, %v", nodes, err)
	}
	w, err := h.st.GetWork(ctx, nodes[0].WorkID)
	if err != nil || w.Trigger != model.TriggerSchedule {
		t.Fatalf("work trigger = %+v, %v", w, err)
	}

	// The run is open: the next occurrence is skipped.
	after, err := h.st.GetWorkflow(ctx, "nightly-flow")
	if err != nil {
		t.Fatal(err)
	}
	h.clock.Advance(after.NextDueAt.Sub(h.clock.Now()) + time.Minute)
	h.srv.scheduleTick(ctx)
	runs, err = h.st.WorkflowRunsFor(ctx, "nightly-flow", 10)
	if err != nil {
		t.Fatal(err)
	}
	if len(runs) != 1 {
		t.Fatalf("skip-if-running violated: %d runs", len(runs))
	}
}

// An invalid cron string is refused at save time — the old silent trap.
func TestScheduleValidatedAtSave(t *testing.T) {
	h := newHarness(t, transportUnix)
	rt := store.Routine{Name: "badcron", Mode: "run", Prompt: "p", Repositories: []string{"equitizr"}, Model: "haiku", TimeoutSeconds: 300,
		Schedule: "not a cron", ScheduleEnabled: true}
	if status, _ := h.do(http.MethodPost, "/api/v1/routines", rt, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("bad routine cron = %d", status)
	}
	h.createRoutine("real")
	wf := store.Workflow{Name: "badwf", Steps: []store.WorkflowStep{{Name: "a", Routine: "real"}}, Schedule: "99 99 * * *"}
	if status, _ := h.do(http.MethodPost, "/api/v1/workflows", wf, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("bad workflow cron = %d", status)
	}
}
