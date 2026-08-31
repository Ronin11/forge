package store

import (
	"testing"
	"time"
)

// setAttemptTimes overwrites an attempt's wall-clock span directly, so a test
// can place it in or out of the timeline window without walking the protocol.
func (f *fixture) setAttemptTimes(id string, started, finished time.Time) {
	f.t.Helper()
	f.write(func(tx *Tx) error {
		_, err := tx.Exec(ctx(), `UPDATE attempts SET started_at = ?, finished_at = ? WHERE id = ?`,
			nullTime(started), nullTime(finished), id)
		return err
	})
}

func TestTimelineItemsWindowAndPhases(t *testing.T) {
	f := newFixture(t)
	since := f.now.Add(-time.Hour)

	// A finished attempt inside the window, with facts (non-zero phases plus a
	// zero phase to drop and a total to exclude).
	_, finTarget := f.newWork("normal")
	finAttempt := f.claim(finTarget, "fin")
	f.setAttemptTimes(finAttempt.ID, f.now.Add(-30*time.Minute), f.now.Add(-10*time.Minute))
	fetch, agent, verify, cleanup, total := int64(1000), int64(5000), int64(800), int64(0), int64(6800)
	f.write(func(tx *Tx) error {
		return tx.InsertFacts(ctx(), &AttemptFacts{
			AttemptID: finAttempt.ID, TargetID: finTarget.ID, WorkID: finTarget.WorkID, Routine: "inventory", Generation: 1,
			Project: "default", Repository: "equitizr", Worker: workerID, Executor: "claude-code", Model: "haiku", Mode: "run",
			Trigger: "manual", Autonomy: "auto", State: "succeeded", FinishedAt: f.now.Add(-10 * time.Minute),
			Phases: map[string]*int64{"fetch": &fetch, "agent": &agent, "verify": &verify, "cleanup": &cleanup, "total": &total},
		})
	})

	// A running attempt inside the window: started, no finished_at, no facts.
	_, runTarget := f.newWork("normal")
	runAttempt := f.claim(runTarget, "run")
	f.setAttemptTimes(runAttempt.ID, f.now.Add(-5*time.Minute), time.Time{})

	// An old finished attempt: finished before the window opened, so excluded.
	_, oldTarget := f.newWork("normal")
	oldAttempt := f.claim(oldTarget, "old")
	f.setAttemptTimes(oldAttempt.ID, f.now.Add(-3*time.Hour), f.now.Add(-2*time.Hour))

	items := must(f.s.TimelineItems(ctx(), since))
	if len(items) != 2 {
		t.Fatalf("TimelineItems = %d items, want 2: %+v", len(items), items)
	}
	// Newest start first: the running attempt (−5m) before the finished (−30m).
	if items[0].AttemptID != runAttempt.ID || items[1].AttemptID != finAttempt.ID {
		t.Fatalf("order = %s, %s; want %s, %s", items[0].AttemptID, items[1].AttemptID, runAttempt.ID, finAttempt.ID)
	}

	run := items[0]
	if !run.FinishedAt.IsZero() {
		t.Errorf("running FinishedAt = %v, want zero", run.FinishedAt)
	}
	if len(run.Phases) != 0 {
		t.Errorf("running Phases = %+v, want none", run.Phases)
	}
	if run.State == "" || run.Repository != "equitizr" || run.Title == "" {
		t.Errorf("running item metadata thin: %+v", run)
	}

	fin := items[1]
	if fin.FinishedAt.IsZero() {
		t.Error("finished FinishedAt is zero")
	}
	want := []TimelinePhase{{Name: "fetch", DurationUS: 1000}, {Name: "agent", DurationUS: 5000}, {Name: "verify", DurationUS: 800}}
	if len(fin.Phases) != len(want) {
		t.Fatalf("finished Phases = %+v, want %+v", fin.Phases, want)
	}
	for i, p := range want {
		if fin.Phases[i] != p {
			t.Errorf("phase %d = %+v, want %+v", i, fin.Phases[i], p)
		}
	}
}
