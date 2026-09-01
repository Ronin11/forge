package web

import (
	"context"
	"encoding/json"
	"net/http"
	"testing"

	"forge/internal/core/store"
)

// An ad-hoc task honours max_turns / timeout_seconds overrides in the body,
// falling back to the ad-hoc defaults when unset.
func TestAdHocTurnAndTimeoutOverride(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)

	snapshotOf := func(id string) store.Routine {
		w, err := h.st.GetWork(context.Background(), id)
		if err != nil {
			t.Fatal(err)
		}
		var rt store.Routine
		if err := json.Unmarshal(w.Snapshot, &rt); err != nil {
			t.Fatalf("snapshot: %v", err)
		}
		return rt
	}

	var out workCreated
	h.call(http.MethodPost, "/api/v1/tasks", map[string]any{
		"prompt": "big multi-file change", "repositories": []string{"equitizr"},
		"max_turns": 99, "timeout_seconds": 600,
	}, &out, http.StatusCreated)
	if rt := snapshotOf(out.Work.ID); rt.MaxTurns != 99 || rt.TimeoutSeconds != 600 {
		t.Fatalf("override not applied: max_turns=%d timeout=%d", rt.MaxTurns, rt.TimeoutSeconds)
	}

	// Unset → the ad-hoc defaults.
	h.call(http.MethodPost, "/api/v1/tasks", map[string]any{
		"prompt": "a small chore", "repositories": []string{"equitizr"},
	}, &out, http.StatusCreated)
	if rt := snapshotOf(out.Work.ID); rt.MaxTurns != adHocMaxTurns || rt.TimeoutSeconds != adHocTimeout {
		t.Fatalf("defaults changed: max_turns=%d timeout=%d", rt.MaxTurns, rt.TimeoutSeconds)
	}
}
