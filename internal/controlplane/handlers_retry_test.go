package controlplane

import (
	"context"
	"net/http"
	"testing"

	"forge/internal/core/model"
	"forge/internal/store"
)

// failedTarget drives one ad-hoc task to a failed Target and returns its id.
func failedTarget(h *harness) string {
	h.t.Helper()
	h.register(testWorkerID)
	var out workCreated
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "do the thing", Repositories: []string{"equitizr"}}, &out, http.StatusCreated)
	c := h.mustClaim("retry-r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 7)
	req := completeRequest(model.Failed, h.clock.Now())
	req.FailureReason = model.ReasonExitNonzero
	h.complete(c, req)
	if tg := h.target(c.TargetID); tg.State != model.Failed {
		h.t.Fatalf("target after failed complete = %+v", tg)
	}
	return c.TargetID
}

func TestRetryEndpoint(t *testing.T) {
	h := newHarness(t, transportUnix)
	targetID := failedTarget(h)

	var got store.Target
	h.call(http.MethodPost, "/api/v1/targets/"+targetID+"/retry", nil, &got, http.StatusOK)
	if got.ID != targetID || got.State != model.Pending || got.FailureReason != "" {
		t.Fatalf("retried target = %+v", got)
	}
	fresh := h.target(targetID)
	if fresh.State != model.Pending || fresh.FailureReason != "" || !fresh.FinishedAt.IsZero() {
		t.Errorf("stored target = %+v", fresh)
	}
	w, err := h.st.GetWork(context.Background(), fresh.WorkID)
	if err != nil {
		t.Fatal(err)
	}
	if !w.FinishedAt.IsZero() {
		t.Errorf("work not reopened: finished at %v", w.FinishedAt)
	}
	// The retried target is claimable again.
	c := h.mustClaim("retry-r2")
	if c.TargetID != targetID {
		t.Errorf("second claim took target %s, want %s", c.TargetID, targetID)
	}
}

func TestRetryEndpointConflict(t *testing.T) {
	h := newHarness(t, transportUnix)
	targetID := failedTarget(h)
	h.call(http.MethodPost, "/api/v1/targets/"+targetID+"/retry", nil, nil, http.StatusOK)
	// pending is not retryable: 409 via model.ErrTransition.
	if status, body := h.do(http.MethodPost, "/api/v1/targets/"+targetID+"/retry", nil, nil, ""); status != http.StatusConflict {
		t.Fatalf("retry of a pending target = %d %s, want 409", status, body)
	}
}

func TestRetryEndpointNotFoundAndBadRequest(t *testing.T) {
	h := newHarness(t, transportUnix)
	targetID := failedTarget(h)
	if status, body := h.do(http.MethodPost, "/api/v1/targets/ffffffffffffffffffffffffffffffff/retry", nil, nil, ""); status != http.StatusNotFound {
		t.Fatalf("unknown target = %d %s, want 404", status, body)
	}
	if status, body := h.do(http.MethodPost, "/api/v1/targets/not-an-id/retry", nil, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("invalid id = %d %s, want 400", status, body)
	}
	if status, body := h.do(http.MethodPost, "/api/v1/targets/"+targetID+"/retry", retryRequest{Model: "sonnet"}, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("model override = %d %s, want 400", status, body)
	}
	if tg := h.target(targetID); tg.State != model.Failed {
		t.Errorf("refused retry changed state to %s", tg.State)
	}
}
