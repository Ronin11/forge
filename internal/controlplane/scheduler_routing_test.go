package controlplane

import (
	"encoding/json"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// A verify Work whose subject has UI=true needs a browser-ready worker; the
// requirements rule (workRequirements) and Pick's capability matching decide
// together at the claim site.
func TestPickBrowserRouting(t *testing.T) {
	snap := json.RawMessage(`{"mode":"verify","executor":"fake-claude","model":"haiku","verify_of":{"attempt_id":"0123456789abcdef0123456789abcdef","branch":"b","head":"h","ui":true}}`)
	w := store.Work{ID: "v", Priority: 50, BudgetClass: model.ClassNormal, Snapshot: snap}
	targets := map[string][]store.Target{"v": {{ID: "tv", WorkID: "v", Repository: "app", State: model.Pending}}}
	order := Order(QueueInput{Work: []store.Work{w}, Targets: targets})
	repos := map[string]store.Repository{"app": {Name: "app", WorkerID: "w1"}}

	missing := store.Worker{ID: "w1", Capabilities: map[string]string{"browser": "missing"}}
	pick := Pick(PickInput{Order: order, Worker: missing, Repositories: repos, Requirements: workRequirements})
	if pick.Target != nil || pick.Skipped["tv"] != "worker lacks capability browser" {
		t.Errorf("browser:missing worker: %+v", pick)
	}

	ready := store.Worker{ID: "w1", Capabilities: map[string]string{"browser": "ready"}}
	pick = Pick(PickInput{Order: order, Worker: ready, Repositories: repos, Requirements: workRequirements})
	if pick.Target == nil || pick.Target.ID != "tv" {
		t.Errorf("browser:ready worker should pick tv: %+v", pick)
	}
}

// The requirements rule stays empty for everything that is not a UI verify.
func TestWorkRequirements(t *testing.T) {
	cases := []struct {
		name string
		snap string
		want int
	}{
		{"ui verify", `{"mode":"verify","verify_of":{"attempt_id":"a","ui":true}}`, 1},
		{"non-ui verify", `{"mode":"verify","verify_of":{"attempt_id":"a","ui":false}}`, 0},
		{"verify without subject", `{"mode":"verify"}`, 0},
		{"ordinary run", `{"mode":"run"}`, 0},
		{"garbage snapshot", `{`, 0},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := workRequirements(store.Work{Snapshot: json.RawMessage(tc.snap)})
			if len(got) != tc.want {
				t.Errorf("requirements = %v, want %d entries", got, tc.want)
			}
			if tc.want == 1 && got[0] != "browser" {
				t.Errorf("requirements = %v, want [browser]", got)
			}
		})
	}
}
