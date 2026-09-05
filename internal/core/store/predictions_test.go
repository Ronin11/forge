package store

import (
	"context"
	"testing"
	"time"
)

// The prediction ledger round-trip: record, list due, resolve, calibrate.
func TestPredictionLedger(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	prob := 0.9
	var held, failed Prediction
	if err := st.Write(ctx, func(tx *Tx) error {
		held = Prediction{Source: "experiment", SourceRef: "e1", ProposalID: "p1", Subject: "directive:x",
			Statement: "v1 survives", Probability: &prob, ResolveBy: time.Now().Add(-time.Hour)}
		if err := tx.InsertPrediction(ctx, &held); err != nil {
			return err
		}
		failed = Prediction{Source: "experiment", SourceRef: "e2", ProposalID: "p2", Subject: "directive:y",
			Statement: "v2 survives", Probability: &prob, ResolveBy: time.Now().Add(time.Hour)}
		return tx.InsertPrediction(ctx, &failed)
	}); err != nil {
		t.Fatal(err)
	}
	due, err := st.DuePredictions(ctx, time.Now())
	if err != nil || len(due) != 2 { // one past due, one proposal-backed
		t.Fatalf("due = %d, %v", len(due), err)
	}
	yes, no := true, false
	if err := st.Write(ctx, func(tx *Tx) error {
		if err := tx.ResolvePrediction(ctx, held.ID, &yes, "survived"); err != nil {
			return err
		}
		return tx.ResolvePrediction(ctx, failed.ID, &no, "reverted")
	}); err != nil {
		t.Fatal(err)
	}
	// Double resolution is refused.
	if err := st.Write(ctx, func(tx *Tx) error {
		return tx.ResolvePrediction(ctx, held.ID, &no, "again")
	}); err == nil {
		t.Fatal("double resolve accepted")
	}
	cal, err := st.Calibration(ctx, time.Now().Add(-30*24*time.Hour))
	if err != nil || len(cal) != 1 {
		t.Fatalf("calibration = %+v, %v", cal, err)
	}
	row := cal[0]
	if row.Source != "experiment" || row.Resolved != 2 || row.Held != 1 || row.Open != 0 {
		t.Fatalf("row = %+v", row)
	}
	// Brier: ((0.9-1)^2 + (0.9-0)^2) / 2 = (0.01 + 0.81) / 2 = 0.41.
	if row.Brier == nil || *row.Brier < 0.40 || *row.Brier > 0.42 {
		t.Fatalf("brier = %v", row.Brier)
	}
}
