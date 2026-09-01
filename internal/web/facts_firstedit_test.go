package web

import (
	"encoding/json"
	"net/http"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

func usageEvent(input, output int64) store.StoredEvent {
	attrs, err := json.Marshal(map[string]int64{"input_tokens": input, "output_tokens": output})
	if err != nil {
		panic(err) // fixed-shape literal; cannot fail
	}
	return store.StoredEvent{Source: protocol.SourceWorker, Event: protocol.Event{Kind: protocol.KindMetric, Name: "usage", Message: "usage", Attrs: attrs}}
}

func toolStart(name string) store.StoredEvent {
	return store.StoredEvent{Source: protocol.SourceWorker, Event: protocol.Event{Kind: protocol.KindSpanStart, Name: name, SpanID: "sp-" + name, ParentID: "agent-1", Message: "tool_use " + name}}
}

func TestComputeTokensToFirstEdit(t *testing.T) {
	now := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	edit := []store.StoredEvent{usageEvent(100, 50), toolStart("Read"), usageEvent(200, 30), toolStart("Edit"), usageEvent(999, 999)}
	f := ComputeFacts(FactsInput{Events: edit, Now: now})
	if f.TokensToFirstEdit == nil || *f.TokensToFirstEdit != 380 {
		t.Errorf("tokens_to_first_edit = %v, want 380", f.TokensToFirstEdit)
	}

	noEdit := []store.StoredEvent{usageEvent(100, 50), toolStart("Read")}
	if f := ComputeFacts(FactsInput{Events: noEdit, Now: now}); f.TokensToFirstEdit != nil {
		t.Errorf("tokens_to_first_edit without an edit = %v, want nil", *f.TokensToFirstEdit)
	}

	firstMessage := []store.StoredEvent{usageEvent(10, 5), toolStart("Write")}
	if f := ComputeFacts(FactsInput{Events: firstMessage, Now: now}); f.TokensToFirstEdit == nil || *f.TokensToFirstEdit != 15 {
		t.Errorf("first-message edit = %v, want 15", f.TokensToFirstEdit)
	}
}

// End to end through the store: the fact survives insert and scan.
func TestTokensToFirstEditRecordedInFacts(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	var out workCreated
	h.call(http.MethodPost, "/api/v1/tasks", workRequest{Prompt: "edit something", Repositories: []string{"equitizr"}}, &out, http.StatusCreated)
	c := h.mustClaim("edit-r1")
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 5)
	now := h.clock.Now()
	batch := protocol.EventBatch{Source: protocol.SourceWorker, Events: []protocol.Event{
		{Seq: 0, Time: now, ElapsedUS: 10, Kind: protocol.KindMetric, Name: "usage", Message: "usage", Attrs: json.RawMessage(`{"input_tokens":10,"output_tokens":5}`)},
		{Seq: 1, Time: now, ElapsedUS: 20, Kind: protocol.KindSpanStart, Name: "Edit", SpanID: "ed1", ParentID: "agent-1", Message: "tool_use Edit"},
	}}
	h.call(http.MethodPost, "/api/v1/attempts/"+c.AttemptID+"/events", batch, nil, http.StatusOK)
	h.complete(c, completeRequest(model.Succeeded, now))
	var detail attemptDetail
	h.call(http.MethodGet, "/api/v1/attempts/"+c.AttemptID, nil, &detail, http.StatusOK)
	if detail.Facts == nil || detail.Facts.TokensToFirstEdit == nil || *detail.Facts.TokensToFirstEdit != 15 {
		t.Errorf("facts tokens_to_first_edit = %+v, want 15", detail.Facts)
	}
}
