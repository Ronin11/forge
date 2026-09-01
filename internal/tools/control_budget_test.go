package tools_test

import (
	"context"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// TestRequestBudget drives forge_request_budget: it validates its input, routes
// to the injected adjudicator, records the request on the ledger, and returns
// the decision inline.
func TestRequestBudget(t *testing.T) {
	f := newFixture(t)
	w, target, a := f.attempt(model.AutonomyAuto, "r1")
	att := toolAttempt(w, target, a)

	// The injected adjudicator stands in for the daemon's child-initiated path:
	// it records the request on the ledger and grants.
	var sawDim string
	var sawAmount float64
	f.deps.Adjudicate = func(ctx context.Context, attemptID, dimension string, amount float64, reason string) (store.BudgetOutcome, error) {
		sawDim, sawAmount = dimension, amount
		if err := f.deps.Write(ctx, func(tx *store.Tx) error {
			_, err := tx.AppendBudgetRequest(ctx, store.BudgetRequest{
				AttemptID: attemptID, Dimension: dimension, Amount: amount, Reason: reason,
				Decision: store.BudgetExtend, GrantedAmount: amount, DecidedBy: "policy", Rationale: "ok",
			})
			return err
		}); err != nil {
			return store.BudgetOutcome{}, err
		}
		return store.BudgetOutcome{Decision: "granted", GrantedAmount: amount, Message: "granted 20 turns"}, nil
	}

	out, err := f.call("forge_request_budget", att, `{"dimension":"turns","amount":20,"reason":"finishing the refactor"}`)
	if err != nil {
		t.Fatal(err)
	}
	if out["decision"] != "granted" || out["granted_amount"] != float64(20) {
		t.Errorf("response = %+v, want granted 20", out)
	}
	if sawDim != "turns" || sawAmount != 20 {
		t.Errorf("adjudicator saw %s/%v, want turns/20", sawDim, sawAmount)
	}
	// The request landed on the ledger.
	ledger, err := f.s.BudgetLedger(ctx(), a.ID)
	if err != nil {
		t.Fatal(err)
	}
	if len(ledger) != 1 || ledger[0].Dimension != "turns" || ledger[0].Decision != store.BudgetExtend {
		t.Errorf("ledger = %+v, want one extend row", ledger)
	}

	// A bad dimension is rejected before the adjudicator is reached.
	if _, err := f.call("forge_request_budget", att, `{"dimension":"vibes","amount":5,"reason":"x"}`); err == nil {
		t.Error("bad dimension accepted")
	}
	// A missing reason is rejected.
	if _, err := f.call("forge_request_budget", att, `{"dimension":"turns","amount":5}`); err == nil {
		t.Error("missing reason accepted")
	}
}
