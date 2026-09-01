package web

import (
	"context"
	"net/http"
	"testing"
)

func TestParseAssistantAction(t *testing.T) {
	if a := parseAssistantAction(`{"action":"status","reply":"ok"}`); a.Action != "status" {
		t.Errorf("plain: %+v", a)
	}
	// Tolerates surrounding prose / fences.
	if a := parseAssistantAction("Sure!\n```json\n{\"action\":\"reply\",\"reply\":\"hi\"}\n```"); a.Action != "reply" || a.Reply != "hi" {
		t.Errorf("wrapped: %+v", a)
	}
	// Unparseable → a plain reply of the raw text.
	if a := parseAssistantAction("just chatting"); a.Action != "reply" || a.Reply != "just chatting" {
		t.Errorf("fallback: %+v", a)
	}
}

func TestAssistantEndpoint(t *testing.T) {
	h := newHarness(t, transportUnix)

	// Disabled without a model call.
	if status, _ := h.do(http.MethodPost, "/api/v1/assistant/message", assistantRequest{Text: "hi"}, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("nil modelCall = %d, want 400", status)
	}

	// A reply action passes the model's text straight through.
	h.srv.modelCall = func(_ context.Context, _, _, _ string) (string, error) {
		return `{"action":"reply","reply":"hello there"}`, nil
	}
	var out assistantResponse
	h.call(http.MethodPost, "/api/v1/assistant/message", assistantRequest{Sender: "s1", Text: "hey"}, &out, http.StatusOK)
	if out.Action != "reply" || out.Reply != "hello there" {
		t.Fatalf("reply = %+v", out)
	}

	// A status action yields a deterministic queue summary (not the model's reply).
	h.srv.modelCall = func(_ context.Context, _, _, _ string) (string, error) {
		return `{"action":"status","reply":"ignored"}`, nil
	}
	h.call(http.MethodPost, "/api/v1/assistant/message", assistantRequest{Sender: "s1", Text: "status?"}, &out, http.StatusOK)
	if out.Action != "status" || out.Reply == "ignored" {
		t.Fatalf("status = %+v", out)
	}

	// Session context: the second call sees the first turn.
	if got := h.srv.assistantUserPrompt("s1", "again"); got == "Operator: again" {
		t.Error("expected prior turns in the prompt")
	}
}
