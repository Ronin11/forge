package store

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
	"time"
)

// Live-kind lifecycle: running → live → one terminal status, with the
// one-open-per-subject unique index and the trim guard that must never delete
// an open row.
func TestLiveExperimentLifecycle(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	arms, err := json.Marshal([]ExperimentArm{{Label: "control", Content: "c"}, {Label: "v1", Content: "v"}})
	if err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(24 * time.Hour)

	pe := Experiment{Subject: "directive:flow-plan", Goal: "g", TargetModel: "haiku", OptimizerModel: "haiku", Kind: ExperimentKindLive}
	if err := st.Write(ctx, func(tx *Tx) error { return tx.InsertExperiment(ctx, &pe) }); err != nil {
		t.Fatal(err)
	}

	// A second open live row on the same subject conflicts; another subject is fine.
	err = st.Write(ctx, func(tx *Tx) error {
		return tx.InsertExperiment(ctx, &Experiment{Subject: "directive:flow-plan", Goal: "g", TargetModel: "haiku", OptimizerModel: "haiku", Kind: ExperimentKindLive})
	})
	if !errors.Is(err, ErrConflict) {
		t.Fatalf("second live insert = %v, want ErrConflict", err)
	}
	other := Experiment{Subject: "directive:other", Goal: "g", TargetModel: "haiku", OptimizerModel: "haiku", Kind: ExperimentKindLive}
	if err := st.Write(ctx, func(tx *Tx) error { return tx.InsertExperiment(ctx, &other) }); err != nil {
		t.Fatal(err)
	}
	// Offline rows on the same subject never collide with the live index.
	if err := st.Write(ctx, func(tx *Tx) error {
		return tx.InsertExperiment(ctx, &Experiment{Subject: "directive:flow-plan", Goal: "g", TargetModel: "haiku", OptimizerModel: "haiku"})
	}); err != nil {
		t.Fatal(err)
	}

	// Deciding a still-running row is illegal; going live first works.
	if err := st.Write(ctx, func(tx *Tx) error {
		return tx.DecideLiveExperiment(ctx, pe.ID, "not-a-status", nil, "")
	}); err == nil {
		t.Fatal("bogus terminal status accepted")
	}
	if err := st.Write(ctx, func(tx *Tx) error {
		return tx.SetExperimentLive(ctx, pe.ID, "control body", arms, 5, deadline)
	}); err != nil {
		t.Fatal(err)
	}
	got, err := st.GetExperiment(ctx, pe.ID)
	if err != nil || got.Status != ExperimentLive || got.MinRuns != 5 || got.OpenedAt.IsZero() || got.DecideBy.IsZero() {
		t.Fatalf("after go-live = %+v, %v", got, err)
	}
	var back []ExperimentArm
	if err := json.Unmarshal(got.Arms, &back); err != nil || len(back) != 2 || back[0].Label != "control" {
		t.Fatalf("arms = %s (%v)", got.Arms, err)
	}
	// SetExperimentLive is running→live only.
	if err := st.Write(ctx, func(tx *Tx) error {
		return tx.SetExperimentLive(ctx, pe.ID, "again", arms, 5, deadline)
	}); err == nil {
		t.Fatal("second go-live accepted")
	}

	// LiveExperiments sees both open rows (one live, one still running setup).
	open, err := st.LiveExperiments(ctx)
	if err != nil || len(open) != 2 {
		t.Fatalf("open = %d, %v", len(open), err)
	}

	// Trim: burying the subject in terminal offline rows must not delete the
	// open live row. Each insert trims terminal history to experimentKeep,
	// then its own finish lands on top — so experimentKeep+1 of the tracked
	// rows survive and at least two are gone.
	var offIDs []string
	for i := 0; i < experimentKeep+3; i++ {
		off := Experiment{Subject: "directive:flow-plan", Goal: "g", TargetModel: "haiku", OptimizerModel: "haiku"}
		if err := st.Write(ctx, func(tx *Tx) error {
			if err := tx.InsertExperiment(ctx, &off); err != nil {
				return err
			}
			return tx.FinishExperiment(ctx, off.ID, ExperimentDone, nil, "")
		}); err != nil {
			t.Fatal(err)
		}
		offIDs = append(offIDs, off.ID)
	}
	if got, err = st.GetExperiment(ctx, pe.ID); err != nil || got.Status != ExperimentLive {
		t.Fatalf("live row after trim = %+v, %v", got, err)
	}
	survivors := 0
	for _, id := range offIDs {
		if _, err := st.GetExperiment(ctx, id); err == nil {
			survivors++
		}
	}
	if survivors != experimentKeep+1 {
		t.Fatalf("terminal survivors after trim = %d, want %d", survivors, experimentKeep+1)
	}

	// Terminal close, then every further decide (double abort etc.) fails.
	res := json.RawMessage(`{"winner":"v1"}`)
	if err := st.Write(ctx, func(tx *Tx) error {
		return tx.DecideLiveExperiment(ctx, pe.ID, ExperimentPromoted, res, "")
	}); err != nil {
		t.Fatal(err)
	}
	if err := st.Write(ctx, func(tx *Tx) error {
		return tx.DecideLiveExperiment(ctx, pe.ID, ExperimentAborted, nil, "")
	}); err == nil {
		t.Fatal("decide on terminal row accepted")
	}
	if got, err = st.GetExperiment(ctx, pe.ID); err != nil || got.Status != ExperimentPromoted || string(got.Results) != string(res) {
		t.Fatalf("closed = %+v, %v", got, err)
	}

	// The freed subject accepts a new live row.
	if err := st.Write(ctx, func(tx *Tx) error {
		return tx.InsertExperiment(ctx, &Experiment{Subject: "directive:flow-plan", Goal: "g2", TargetModel: "haiku", OptimizerModel: "haiku", Kind: ExperimentKindLive})
	}); err != nil {
		t.Fatalf("reopen after close: %v", err)
	}
}
