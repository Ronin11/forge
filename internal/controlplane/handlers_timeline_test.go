package controlplane

import (
	"net/http"
	"testing"
	"time"

	"forge/internal/model"
	"forge/internal/protocol"
)

// TestTimelineEndpoint drives one attempt to completion, then asserts the
// timeline endpoint's shape, its phase breakdown, the garbage-window 400, and
// the 24h clamp.
func TestTimelineEndpoint(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	h.run("inventory")
	c := h.mustClaim("r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 4242)
	now := h.clock.Now()
	batch := protocol.EventBatch{Source: protocol.SourceWorker, Events: []protocol.Event{
		{Seq: 0, Time: now, Kind: protocol.KindSpanStart, Name: "agent-1", SpanID: "agent-1"},
		{Seq: 1, Time: now, ElapsedUS: 500, Kind: protocol.KindSpanEnd, Name: "agent-1", SpanID: "agent-1", DurationUS: 500},
	}}
	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/events", batch, nil, http.StatusOK)
	if done := h.complete(c, completeRequest(model.Succeeded, now)); done.State != model.Succeeded {
		t.Fatalf("complete = %+v", done)
	}

	// Default-ish window: the attempt finished at now, well inside one hour.
	var body timelineResponse
	h.call(http.MethodGet, "/api/v1/timeline?window=1h", nil, &body, http.StatusOK)
	if body.WindowSeconds != int(time.Hour/time.Second) {
		t.Errorf("window_seconds = %d, want 3600", body.WindowSeconds)
	}
	if body.Now.IsZero() || body.Since.IsZero() || !body.Since.Equal(body.Now.Add(-time.Hour)) {
		t.Errorf("now/since = %v / %v", body.Now, body.Since)
	}
	if len(body.Items) != 1 {
		t.Fatalf("items = %d, want 1: %+v", len(body.Items), body.Items)
	}
	it := body.Items[0]
	if it.AttemptID != c.AttemptID || it.WorkID != c.WorkID || it.Title == "" || it.State != string(model.Succeeded) {
		t.Errorf("item = %+v", it)
	}
	if it.FinishedAt.IsZero() {
		t.Error("finished item has zero FinishedAt")
	}
	var hasAgent bool
	for _, p := range it.Phases {
		if p.Name == "agent" && p.DurationUS == 500 {
			hasAgent = true
		}
	}
	if !hasAgent {
		t.Errorf("phases missing agent=500: %+v", it.Phases)
	}

	// Garbage window is a 400.
	h.call(http.MethodGet, "/api/v1/timeline?window=nonsense", nil, nil, http.StatusBadRequest)

	// An over-long window clamps to 24h.
	var clamped timelineResponse
	h.call(http.MethodGet, "/api/v1/timeline?window=100h", nil, &clamped, http.StatusOK)
	if clamped.WindowSeconds != int(24*time.Hour/time.Second) {
		t.Errorf("clamped window_seconds = %d, want 86400", clamped.WindowSeconds)
	}
}
