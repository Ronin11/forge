package controlplane

import (
	"bytes"
	"context"
	"net/http"
	"testing"
	"time"
)

// Intake dedupe (M11): an identical ad-hoc prompt within 24 h is refused with
// 409 and journaled; --force overrides; the window expires; routine reruns
// are never deduplicated.
func TestIntakeDedupe(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)

	req := workRequest{Prompt: "fix the flaky test", Repositories: []string{"equitizr"}}
	var first workCreated
	h.call(http.MethodPost, "/api/v1/tasks", req, &first, http.StatusCreated)
	if first.Work.PromptHash == "" {
		t.Error("prompt_hash not populated at create")
	}

	status, raw := h.do(http.MethodPost, "/api/v1/tasks", req, nil, "")
	if status != http.StatusConflict || !bytes.Contains(raw, []byte("duplicate")) {
		t.Fatalf("duplicate submit = %d %s, want 409", status, raw)
	}
	entries, err := h.st.JournalForEntity(context.Background(), "work", first.Work.ID)
	if err != nil || !hasKind(entries, "work.deduplicated") {
		t.Errorf("journal lacks work.deduplicated (%v)", err)
	}

	// Normalization: surrounding whitespace does not evade the hash.
	padded := req
	padded.Prompt = "  fix the flaky test\n"
	if status, _ := h.do(http.MethodPost, "/api/v1/tasks", padded, nil, ""); status != http.StatusConflict {
		t.Errorf("padded duplicate = %d, want 409", status)
	}

	forced := req
	forced.Force = true
	h.call(http.MethodPost, "/api/v1/tasks", forced, nil, http.StatusCreated)

	other := workRequest{Prompt: "another thing entirely", Repositories: []string{"equitizr"}}
	h.call(http.MethodPost, "/api/v1/tasks", other, nil, http.StatusCreated)

	// The window: 25 h later the same prompt is fresh work again.
	h.clock.Advance(25 * time.Hour)
	h.register(testWorkerID) // keep the worker connected under the advanced clock
	h.call(http.MethodPost, "/api/v1/tasks", req, nil, http.StatusCreated)

	// Routine runs carry a stable prompt by design; reruns are not refused.
	h.createRoutine("daily")
	h.run("daily")
	h.run("daily")
}
