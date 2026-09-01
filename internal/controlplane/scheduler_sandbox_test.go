package controlplane

import (
	"encoding/json"
	"testing"

	"forge/internal/core/engine"
	"forge/internal/core/model"
	"forge/internal/core/store"
)

// A require_sandbox routine routes only to a sandbox-ready worker; the
// requirements rule and engine.Pick's capability matching decide at the claim site,
// mirroring the browser rule of M8.
func TestPickSandboxRouting(t *testing.T) {
	snap := json.RawMessage(`{"mode":"run","executor":"fake-claude","model":"haiku","require_sandbox":true}`)
	w := store.Work{ID: "s", Priority: 50, BudgetClass: model.ClassNormal, Snapshot: snap}
	targets := map[string][]store.Target{"s": {{ID: "ts", WorkID: "s", Repository: "app", State: model.Pending}}}
	order := engine.Order(engine.QueueInput{Work: []store.Work{w}, Targets: targets})
	repos := map[string]store.Repository{"app": {Name: "app", WorkerID: "w1"}}

	missing := store.Worker{ID: "w1", Capabilities: map[string]string{"sandbox": "missing"}}
	pick := engine.Pick(engine.PickInput{Order: order, Worker: missing, Repositories: repos, Requirements: workRequirements})
	if pick.Target != nil || pick.Skipped["ts"] != "worker lacks capability sandbox" {
		t.Errorf("sandbox:missing worker: %+v", pick)
	}

	ready := store.Worker{ID: "w1", Capabilities: map[string]string{"sandbox": "ready"}}
	pick = engine.Pick(engine.PickInput{Order: order, Worker: ready, Repositories: repos, Requirements: workRequirements})
	if pick.Target == nil || pick.Target.ID != "ts" {
		t.Errorf("sandbox:ready worker should pick ts: %+v", pick)
	}
}

// The requirements rule composes: a sandboxed UI verify needs both.
func TestWorkRequirementsSandbox(t *testing.T) {
	cases := []struct {
		name string
		snap string
		want []string
	}{
		{"require_sandbox run", `{"mode":"run","require_sandbox":true}`, []string{"sandbox"}},
		{"plain run", `{"mode":"run"}`, nil},
		{"sandboxed ui verify", `{"mode":"verify","require_sandbox":true,"verify_of":{"attempt_id":"a","ui":true}}`, []string{"sandbox", "browser"}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := workRequirements(store.Work{Snapshot: json.RawMessage(tc.snap)})
			if len(got) != len(tc.want) {
				t.Fatalf("requirements = %v, want %v", got, tc.want)
			}
			for i := range got {
				if got[i] != tc.want[i] {
					t.Errorf("requirements = %v, want %v", got, tc.want)
				}
			}
		})
	}
}
