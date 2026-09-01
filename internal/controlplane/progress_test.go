package controlplane

import (
	"encoding/json"
	"net/http"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
)

// TestRunningAttemptLiveProgress checks that the task view surfaces a running
// attempt's live tally: turns/tokens derived from usage events, the heartbeat
// phase/state, the last-event time, and the latest progress note.
func TestRunningAttemptLiveProgress(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	h.run("inventory")
	c := h.mustClaim("r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 4242)

	now := h.clock.Now()
	batch := protocol.EventBatch{Source: protocol.SourceWorker, Events: []protocol.Event{
		{Seq: 10, Time: now, Kind: protocol.KindMetric, Name: "usage", Message: "usage m1", Attrs: json.RawMessage(`{"message_id":"m1","input_tokens":100,"output_tokens":40}`)},
		{Seq: 11, Time: now, Kind: protocol.KindMetric, Name: "usage", Message: "usage m2", Attrs: json.RawMessage(`{"message_id":"m2","input_tokens":80,"output_tokens":112}`)},
	}}
	var ins map[string]int
	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/events", batch, &ins, http.StatusOK)

	note := protocol.EventBatch{Source: protocol.SourceControl, Events: []protocol.Event{
		{Seq: 1, Time: now, Kind: protocol.KindLifecycle, Name: "progress", Message: "building and verifying", Attrs: json.RawMessage(`{"checkpoint":"after_plan"}`)},
	}}
	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/events", note, nil, http.StatusOK)

	var wd workDetail
	h.call(http.MethodGet, "/api/v1/tasks/"+c.WorkID, nil, &wd, http.StatusOK)
	if len(wd.Attempts) != 1 {
		t.Fatalf("attempts = %d, want 1", len(wd.Attempts))
	}
	p := wd.Attempts[0].Progress
	if p == nil {
		t.Fatal("running attempt has no live progress")
	}
	if p.RunningTurns != 2 || p.TokensIn != 180 || p.TokensOut != 152 {
		t.Errorf("tally = turns %d tokens %d/%d, want 2 180/152", p.RunningTurns, p.TokensIn, p.TokensOut)
	}
	if p.State != model.Running || p.Phase != "running" {
		t.Errorf("state/phase = %q/%q, want running/running", p.State, p.Phase)
	}
	if p.PhaseAt.IsZero() || p.LastEventAt.IsZero() {
		t.Errorf("phase_at %v / last_event_at %v must be set", p.PhaseAt, p.LastEventAt)
	}
	if p.Note != "building and verifying" || p.Checkpoint != "after_plan" {
		t.Errorf("note/checkpoint = %q/%q", p.Note, p.Checkpoint)
	}

	// Once the attempt completes, the view stops attaching the live tally — the
	// authoritative totals live on the attempt itself.
	h.complete(c, completeRequest(model.Succeeded, now))
	var done workDetail
	h.call(http.MethodGet, "/api/v1/tasks/"+c.WorkID, nil, &done, http.StatusOK)
	if len(done.Attempts) != 1 || done.Attempts[0].Progress != nil {
		t.Errorf("completed attempt still carries live progress: %+v", done.Attempts[0].Progress)
	}
}
