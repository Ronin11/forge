package controlplane

import (
	"encoding/json"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// A paused repository admits no new work: Pick skips its target with reason
// repository_paused; resuming (paused=false) admits it.
func TestPickRepositoryPaused(t *testing.T) {
	snap := json.RawMessage(`{"mode":"run","executor":"fake-claude","model":"haiku"}`)
	w := store.Work{ID: "s", Priority: 50, BudgetClass: model.ClassNormal, Snapshot: snap}
	targets := map[string][]store.Target{"s": {{ID: "ts", WorkID: "s", Repository: "app", State: model.Pending}}}
	order := Order(QueueInput{Work: []store.Work{w}, Targets: targets})
	worker := store.Worker{ID: "w1"}

	paused := map[string]store.Repository{"app": {Name: "app", WorkerID: "w1", Paused: true}}
	pick := Pick(PickInput{Order: order, Worker: worker, Repositories: paused})
	if pick.Target != nil {
		t.Errorf("paused repository should admit nothing: %+v", pick.Target)
	}
	if pick.Skipped["ts"] != "repository_paused" {
		t.Errorf("skip reason = %q, want repository_paused", pick.Skipped["ts"])
	}

	live := map[string]store.Repository{"app": {Name: "app", WorkerID: "w1"}}
	pick = Pick(PickInput{Order: order, Worker: worker, Repositories: live})
	if pick.Target == nil || pick.Target.ID != "ts" {
		t.Errorf("unpaused repository should admit ts: %+v", pick)
	}
}
