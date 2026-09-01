package controlplane

import (
	"context"
	"net/http"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
)

// runningAttempt drives one ad-hoc task to a running Target and returns its claim.
func runningAttempt(h *harness) *protocol.Claim {
	h.t.Helper()
	h.register(testWorkerID)
	var out workCreated
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "steer me", Repositories: []string{"equitizr"}}, &out, http.StatusCreated)
	c := h.mustClaim("steer-r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 7)
	return c
}

func TestSteerQueuedAndDeliveredOnHeartbeat(t *testing.T) {
	h := newHarness(t, transportUnix)
	c := runningAttempt(h)

	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/steer", steerRequest{Text: "focus on the parser"}, nil, http.StatusAccepted)
	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/steer", steerRequest{Text: "and add a test"}, nil, http.StatusAccepted)

	hb := h.heartbeat(c, "", 0)
	if len(hb.Steer) != 2 || hb.Steer[0] != "focus on the parser" || hb.Steer[1] != "and add a test" {
		t.Fatalf("heartbeat steer = %v", hb.Steer)
	}
	// Delivered means cleared: the next heartbeat carries nothing.
	if hb = h.heartbeat(c, "", 0); len(hb.Steer) != 0 {
		t.Errorf("second heartbeat steer = %v, want none", hb.Steer)
	}
	entries, err := h.st.JournalForEntity(context.Background(), "attempt", c.AttemptID)
	if err != nil {
		t.Fatal(err)
	}
	if !hasKind(entries, "attempt.steer") || !hasKind(entries, "attempt.steer_delivered") {
		t.Errorf("journal lacks steer rows: %+v", entries)
	}

	// A steer queued later is delivered later.
	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/steer", steerRequest{Text: "one more"}, nil, http.StatusAccepted)
	if hb = h.heartbeat(c, "", 0); len(hb.Steer) != 1 || hb.Steer[0] != "one more" {
		t.Errorf("third heartbeat steer = %v", hb.Steer)
	}

	// Empty text is the client's mistake.
	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/steer", steerRequest{Text: "  "}, nil, http.StatusBadRequest)
}

func TestSteerRefusedWhenNotRunning(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	var out workCreated
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "steer target states", Repositories: []string{"equitizr"}}, &out, http.StatusCreated)
	c := h.mustClaim("steer-s1")

	// claimed and preparing are not running: 409.
	if status, body := h.do(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/steer", steerRequest{Text: "x"}, nil, ""); status != http.StatusConflict {
		t.Fatalf("steer while claimed = %d %s", status, body)
	}
	h.heartbeat(c, model.Preparing, 0)
	if status, _ := h.do(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/steer", steerRequest{Text: "x"}, nil, ""); status != http.StatusConflict {
		t.Fatalf("steer while preparing = %d", status)
	}
	h.heartbeat(c, model.Running, 3)
	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/steer", steerRequest{Text: "x"}, nil, http.StatusAccepted)

	h.complete(c, completeRequest(model.Succeeded, h.clock.Now()))
	if status, _ := h.do(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/steer", steerRequest{Text: "x"}, nil, ""); status != http.StatusConflict {
		t.Fatalf("steer after completion = %d", status)
	}
	if status, _ := h.do(http.MethodPost, "/api/v1/attempts/ffffffffffffffffffffffffffffffff/steer", steerRequest{Text: "x"}, nil, ""); status != http.StatusNotFound {
		t.Errorf("steer of unknown attempt = %d", status)
	}
	if status, _ := h.do(http.MethodPost, "/api/v1/attempts/not-an-id/steer", steerRequest{Text: "x"}, nil, ""); status != http.StatusBadRequest {
		t.Errorf("steer of invalid id = %d", status)
	}
}
