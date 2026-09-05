package stats

import (
	"testing"
	"time"

	"forge/internal/core/store"
)

func iptr(v int) *int         { return &v }
func fptr(v float64) *float64 { return &v }
func bptr(v bool) *bool       { return &v }
func i64ptr(v int64) *int64   { return &v }

// The size-calibration table: buckets in fixed order, cost/turns/score per
// bucket, unsized rows collected under "unsized", empty buckets omitted.
func TestSizeBuckets(t *testing.T) {
	now := time.Date(2026, 9, 4, 12, 0, 0, 0, time.UTC)
	rows := []store.AttemptFacts{
		{Size: "S", State: "succeeded", VerificationPass: bptr(true), CostUSD: fptr(0.10), Turns: iptr(10), Phases: map[string]*int64{"total": i64ptr(1000)}, FinishedAt: now},
		{Size: "S", State: "failed", CostUSD: fptr(0.30), Turns: iptr(30), Phases: map[string]*int64{}, FinishedAt: now},
		{Size: "L", State: "succeeded", VerificationPass: bptr(true), CostUSD: fptr(2.00), Turns: iptr(50), Phases: map[string]*int64{}, ScoreOverall: iptr(4), FinishedAt: now},
		{State: "succeeded", Phases: map[string]*int64{}, FinishedAt: now}, // unsized, cost unknown
	}
	got := sizeBuckets(rows)
	if len(got) != 3 || got[0].Size != "S" || got[1].Size != "L" || got[2].Size != "unsized" {
		t.Fatalf("buckets = %+v", got)
	}
	s := got[0]
	if s.Runs != 2 || s.VerifiedSuccesses != 1 || s.VerifiedSuccessRate != 0.5 || s.CostPerRun != 0.20 || s.TurnsPerRun != 20 || s.DurationP50US != 1000 {
		t.Errorf("S = %+v", s)
	}
	if got[1].ScoreOverallMean == nil || *got[1].ScoreOverallMean != 4 {
		t.Errorf("L score = %+v", got[1].ScoreOverallMean)
	}
	if got[2].CostPerRun != 0 || got[2].ScoreOverallMean != nil {
		t.Errorf("unsized = %+v", got[2])
	}
	// The query filter narrows to one bucket, with "unsized" as the empty alias.
	if f := filter(rows, Query{Size: "S"}); len(f) != 2 {
		t.Errorf("filter S = %d rows", len(f))
	}
	if f := filter(rows, Query{Size: "unsized"}); len(f) != 1 {
		t.Errorf("filter unsized = %d rows", len(f))
	}
}
