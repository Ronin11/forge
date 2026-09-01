package store

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
	"time"

	"forge/internal/core/model"
)

func createTestWorkflow(t *testing.T, s *Store, wf *Workflow) {
	t.Helper()
	if err := s.Write(context.Background(), func(tx *Tx) error { return tx.CreateWorkflow(context.Background(), wf) }); err != nil {
		t.Fatal(err)
	}
}

func TestWorkflowCreateNormalizesChain(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	wf := &Workflow{Name: "nightly", Steps: []WorkflowStep{
		{Name: "lint", Routine: "lint-all"},
		{Name: "fix", Routine: "fix-lint"},
		{Name: "verify", Routine: "verify-all", After: []WorkflowEdge{{Step: "lint"}, {Step: "fix", StackOn: true}}},
	}}
	createTestWorkflow(t, s, wf)
	got, err := s.GetWorkflow(ctx, "nightly")
	if err != nil {
		t.Fatal(err)
	}
	if got.Generation != 1 || len(got.Steps) != 3 {
		t.Fatalf("workflow = %+v", got)
	}
	// A step without `after` follows the previous one; empty `on` becomes success.
	if a := got.Steps[1].After; len(a) != 1 || a[0].Step != "lint" || a[0].On != model.OnSuccess {
		t.Errorf("fix.after = %+v, want the previous step on success", a)
	}
	if a := got.Steps[2].After; len(a) != 2 || a[0].On != model.OnSuccess || !a[1].StackOn {
		t.Errorf("verify.after = %+v", a)
	}
	// The generation snapshot exists and round-trips.
	var snap string
	if err := s.queryRow(ctx, `SELECT snapshot FROM workflow_generations WHERE workflow_id = ? AND generation = 1`, got.ID).Scan(&snap); err != nil {
		t.Fatal(err)
	}
	var decoded Workflow
	if err := json.Unmarshal([]byte(snap), &decoded); err != nil || decoded.Name != "nightly" {
		t.Errorf("snapshot = %s, %v", snap, err)
	}
}

func TestWorkflowValidate(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	cases := []struct {
		name string
		wf   Workflow
	}{
		{"no steps", Workflow{Name: "empty"}},
		{"bad step name", Workflow{Name: "bad", Steps: []WorkflowStep{{Name: "Bad Name", Routine: "r"}}}},
		{"bad routine name", Workflow{Name: "bad", Steps: []WorkflowStep{{Name: "a", Routine: "No!"}}}},
		{"duplicate step", Workflow{Name: "dup", Steps: []WorkflowStep{{Name: "a", Routine: "r"}, {Name: "a", Routine: "r"}}}},
		{"forward reference", Workflow{Name: "fwd", Steps: []WorkflowStep{{Name: "a", Routine: "r", After: []WorkflowEdge{{Step: "b"}}}, {Name: "b", Routine: "r"}}}},
		{"self reference", Workflow{Name: "self", Steps: []WorkflowStep{{Name: "a", Routine: "r", After: []WorkflowEdge{{Step: "a"}}}}}},
		{"bad on", Workflow{Name: "on", Steps: []WorkflowStep{{Name: "a", Routine: "r"}, {Name: "b", Routine: "r", After: []WorkflowEdge{{Step: "a", On: "sometimes"}}}}}},
	}
	for _, tc := range cases {
		wf := tc.wf
		err := s.Write(ctx, func(tx *Tx) error { return tx.CreateWorkflow(ctx, &wf) })
		if err == nil {
			t.Errorf("%s: created", tc.name)
		}
	}
}

func TestWorkflowUpdateGenerations(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	wf := &Workflow{Name: "wf", Steps: []WorkflowStep{{Name: "a", Routine: "r-a"}}}
	createTestWorkflow(t, s, wf)
	dup := &Workflow{Name: "wf", Steps: wf.Steps}
	if err := s.Write(ctx, func(tx *Tx) error { return tx.CreateWorkflow(ctx, dup) }); !errors.Is(err, ErrConflict) {
		t.Fatalf("duplicate create = %v, want conflict", err)
	}
	wf.Steps = append(wf.Steps, WorkflowStep{Name: "b", Routine: "r-b"})
	if err := s.Write(ctx, func(tx *Tx) error { return tx.UpdateWorkflow(ctx, wf, 1) }); err != nil {
		t.Fatal(err)
	}
	if wf.Generation != 2 {
		t.Fatalf("generation = %d, want 2", wf.Generation)
	}
	if err := s.Write(ctx, func(tx *Tx) error { return tx.UpdateWorkflow(ctx, wf, 1) }); !errors.Is(err, ErrStaleGeneration) {
		t.Fatalf("stale update = %v", err)
	}
	if err := s.Write(ctx, func(tx *Tx) error { return tx.UpdateWorkflow(ctx, &Workflow{Name: "ghost", Steps: wf.Steps}, 1) }); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing update = %v", err)
	}
	var n int
	if err := s.queryRow(ctx, `SELECT count(*) FROM workflow_generations WHERE workflow_id = ?`, wf.ID).Scan(&n); err != nil || n != 2 {
		t.Errorf("generation records = %d, %v", n, err)
	}
}

func TestWorkflowArchiveAndDue(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	wf := &Workflow{Name: "cron", Steps: []WorkflowStep{{Name: "a", Routine: "r"}}, Schedule: "0 3 * * *", ScheduleEnabled: true}
	createTestWorkflow(t, s, wf)
	due := time.Date(2026, 8, 30, 3, 0, 0, 0, time.UTC)
	if err := s.Write(ctx, func(tx *Tx) error { return tx.SetWorkflowNextDue(ctx, "cron", due) }); err != nil {
		t.Fatal(err)
	}
	got, err := s.DueWorkflows(ctx, due.Add(time.Minute))
	if err != nil || len(got) != 1 || got[0].Name != "cron" {
		t.Fatalf("due = %+v, %v", got, err)
	}
	if err := s.Write(ctx, func(tx *Tx) error { return tx.ArchiveWorkflow(ctx, "cron") }); err != nil {
		t.Fatal(err)
	}
	if got, err = s.DueWorkflows(ctx, due.Add(time.Minute)); err != nil || len(got) != 0 {
		t.Fatalf("archived workflow still due: %+v, %v", got, err)
	}
	// Archived: hidden from the default list, visible when asked, second archive 404s.
	if ws, err := s.ListWorkflows(ctx, false); err != nil || len(ws) != 0 {
		t.Fatalf("list = %+v, %v", ws, err)
	}
	if ws, err := s.ListWorkflows(ctx, true); err != nil || len(ws) != 1 || ws[0].ArchivedAt.IsZero() {
		t.Fatalf("list archived = %+v, %v", ws, err)
	}
	if err := s.Write(ctx, func(tx *Tx) error { return tx.ArchiveWorkflow(ctx, "cron") }); !errors.Is(err, ErrNotFound) {
		t.Fatalf("second archive = %v", err)
	}
}
