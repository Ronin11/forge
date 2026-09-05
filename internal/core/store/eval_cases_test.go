package store

import (
	"context"
	"testing"
	"time"
)

// Eval cases persist per run and read back newest first — the time series
// behind proposals.eval_score.
func TestEvalCasesRoundTrip(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	turns, cost := 3, 0.02
	if err := st.Write(ctx, func(tx *Tx) error {
		return tx.InsertEvalCases(ctx, []EvalCase{
			{ProposalID: "p1", Mode: "run", CaseName: "inventory", Pass: true, State: "succeeded", Turns: &turns, CostUSD: &cost},
			{ProposalID: "p1", Mode: "run", CaseName: "failing", Pass: false, State: "failed", FailureReason: "state", Details: "want failed"},
		})
	}); err != nil {
		t.Fatal(err)
	}
	got, err := st.ListEvalCases(ctx, "run", 10)
	if err != nil || len(got) != 2 {
		t.Fatalf("list = %d, %v", len(got), err)
	}
	byName := map[string]EvalCase{}
	for _, c := range got {
		byName[c.CaseName] = c
		if c.ProposalID != "p1" || c.CreatedAt.IsZero() || time.Since(c.CreatedAt) < 0 {
			t.Errorf("case = %+v", c)
		}
	}
	if !byName["inventory"].Pass || byName["inventory"].Turns == nil || *byName["inventory"].Turns != 3 {
		t.Errorf("inventory = %+v", byName["inventory"])
	}
	if byName["failing"].Pass || byName["failing"].FailureReason != "state" {
		t.Errorf("failing = %+v", byName["failing"])
	}
	if other, err := st.ListEvalCases(ctx, "plan", 10); err != nil || len(other) != 0 {
		t.Errorf("mode filter = %d, %v", len(other), err)
	}
}
