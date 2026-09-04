package store

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
)

func TestParseTarget(t *testing.T) {
	for _, tc := range []struct {
		in   string
		kind TargetKind
		name string
		bad  bool
	}{
		{in: "", kind: "", name: ""},
		{in: "directive:triage", kind: TargetDirective, name: "triage"},
		{in: "directive:roles/reviewer", kind: TargetDirective, name: "roles/reviewer"},
		{in: "workflow:release", kind: TargetWorkflow, name: "release"},
		{in: "workflow:has/slash", bad: true},
		{in: "directive:", bad: true},
		{in: "routine:x", bad: true},
		{in: "directive:Bad Name", bad: true},
		{in: "triage", bad: true},
	} {
		kind, name, err := ParseTarget(tc.in)
		if tc.bad {
			if err == nil {
				t.Errorf("ParseTarget(%q) accepted", tc.in)
			}
			continue
		}
		if err != nil || kind != tc.kind || name != tc.name {
			t.Errorf("ParseTarget(%q) = %q %q %v", tc.in, kind, name, err)
		}
	}
}

// A routine is exactly one of two shapes: trigger (target, no content) or
// legacy content. Both round-trip through the store.
func TestTargetRoutineRoundTrip(t *testing.T) {
	s := openTest(t)
	bg := context.Background()
	r := &Routine{Name: "nightly", Target: "directive:triage", Objective: "sweep the queues",
		Repositories: []string{"equitizr"}, Schedule: "0 3 * * *", ScheduleEnabled: true}
	if err := s.Write(bg, func(tx *Tx) error { return tx.CreateRoutine(bg, r) }); err != nil {
		t.Fatal(err)
	}
	got, err := s.GetRoutine(bg, "nightly")
	if err != nil {
		t.Fatal(err)
	}
	if got.Target != "directive:triage" || got.Objective != "sweep the queues" || got.Prompt != "" || got.Mode != "" {
		t.Fatalf("round trip = %+v", got)
	}
	if got.TimeoutSeconds != 3600 {
		t.Errorf("target default timeout = %d", got.TimeoutSeconds)
	}

	// Update keeps the shape; generation snapshot carries the target.
	got.Objective = "sweep harder"
	if err := s.Write(bg, func(tx *Tx) error { return tx.UpdateRoutine(bg, got, 1) }); err != nil {
		t.Fatal(err)
	}
	var snap json.RawMessage
	err = s.Write(bg, func(tx *Tx) error {
		var err error
		snap, err = tx.GenerationSnapshot(bg, got.ID, 2)
		return err
	})
	if err != nil || !strings.Contains(string(snap), `"target":"directive:triage"`) {
		t.Fatalf("generation snapshot = %s, %v", snap, err)
	}
}

func TestTargetRoutineValidate(t *testing.T) {
	base := Routine{Name: "t", Repositories: []string{"equitizr"}, TimeoutSeconds: 300, Concurrency: 1, BudgetClass: "normal", Executor: "claude-code", MaxQuestions: 1, Priority: 50}
	target := base
	target.Target = "directive:triage"
	if err := target.Validate(); err != nil {
		t.Errorf("target routine rejected: %v", err)
	}
	both := target
	both.Prompt = "sneaky"
	if err := both.Validate(); err == nil || !strings.Contains(err.Error(), "no content fields") {
		t.Errorf("target+content accepted: %v", err)
	}
	badKind := base
	badKind.Target = "prompt:triage"
	if err := badKind.Validate(); err == nil {
		t.Error("bad target kind accepted")
	}
	// Legacy content rules unchanged.
	legacy := base
	legacy.Mode, legacy.Prompt, legacy.Model = "run", "do it", "haiku"
	if err := legacy.Validate(); err != nil {
		t.Errorf("legacy routine rejected: %v", err)
	}
	if err := base.Validate(); err == nil {
		t.Error("empty routine accepted")
	}
}

// Pre-target snapshots (no target/objective keys) decode into today's struct.
func TestLegacySnapshotDecode(t *testing.T) {
	old := `{"id":"x","name":"inventory","mode":"run","prompt":"list files","model":"haiku","repositories":["equitizr"],"executor":"claude-code","timeout_seconds":300,"priority":50,"budget_class":"normal","concurrency":1,"integrate":false,"require_sandbox":false,"max_questions":3,"generation":4,"schedule_enabled":false,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}`
	var r Routine
	if err := json.Unmarshal([]byte(old), &r); err != nil {
		t.Fatal(err)
	}
	if r.Target != "" || r.Prompt != "list files" || r.Generation != 4 {
		t.Errorf("decoded = %+v", r)
	}
	if err := r.Validate(); err != nil {
		t.Errorf("legacy snapshot no longer validates: %v", err)
	}
}
