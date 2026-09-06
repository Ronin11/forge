package web

import (
	"context"
	"net/http"
	"strings"
	"testing"
	"time"
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

	// Session context: the second call sees the first turn — from the STORE,
	// so it survives what a daemon restart used to wipe.
	if got := h.srv.assistantUserPrompt(context.Background(), "s1", "again"); !strings.Contains(got, "hello there") {
		t.Errorf("expected prior turns in the prompt: %q", got)
	}
}

// Sessionization: referents ride into the prompt with live state, and an
// idle gap closes the session down to a one-line bridge.
func TestAssistantSessionContext(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.srv.modelCall = func(_ context.Context, _, user, _ string) (string, error) {
		return `{"action":"create_task","prompt":"fix the flaky test","repo":"equitizr","reply":"On it."}`, nil
	}
	var out assistantResponse
	h.call(http.MethodPost, "/api/v1/assistant/message", assistantRequest{Sender: "nate", Text: "fix that flaky test in equitizr"}, &out, http.StatusOK)
	if out.Action != "create_task" || !strings.Contains(out.Reply, "task ") {
		t.Fatalf("create = %+v", out)
	}

	// The referent appears with live state, and the transcript is present.
	prompt := h.srv.assistantUserPrompt(context.Background(), "nate", "how did it go?")
	if !strings.Contains(prompt, "Ongoing from this chat") || !strings.Contains(prompt, "fix the flaky test") {
		t.Fatalf("no referent in prompt:\n%s", prompt)
	}
	if !strings.Contains(prompt, "pending") && !strings.Contains(prompt, "running") {
		t.Fatalf("referent lacks live state:\n%s", prompt)
	}

	// An idle gap collapses the session to the bridge line; the referent
	// still rides (previous session's entities stay addressable).
	h.clock.Advance(2 * time.Hour)
	prompt = h.srv.assistantUserPrompt(context.Background(), "nate", "morning")
	if !strings.Contains(prompt, "Previous session ended") {
		t.Fatalf("no session bridge after idle gap:\n%s", prompt)
	}
	if strings.Count(prompt, "\nOperator: ") > 1 { // only the new message; the old turn survives solely in the bridge line
		t.Fatalf("old transcript leaked into the new session:\n%s", prompt)
	}
	if !strings.Contains(prompt, "Ongoing from this chat") {
		t.Fatalf("referents dropped across sessions:\n%s", prompt)
	}
}
