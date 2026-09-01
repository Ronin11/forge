package web

import (
	"context"
	"encoding/json"
	"net/http"
	"testing"
)

func TestRPCNotifyTest(t *testing.T) {
	h := newHarness(t, transportUnix)
	var out struct {
		Queued bool   `json:"queued"`
		Path   string `json:"path"`
	}

	// No body: the toast routes to the Human queue.
	h.call(http.MethodPost, "/api/v1/rpc/notify.test", nil, &out, http.StatusOK)
	if !out.Queued || out.Path != "/attention" {
		t.Errorf("default = %+v", out)
	}

	// Args may pick the page the click should open.
	h.call(http.MethodPost, "/api/v1/rpc/notify.test", map[string]any{"path": "/tasks/abc"}, &out, http.StatusOK)
	if out.Path != "/tasks/abc" {
		t.Errorf("path = %q", out.Path)
	}

	// Only UI paths: an absolute or protocol-relative URL is refused.
	h.call(http.MethodPost, "/api/v1/rpc/notify.test", map[string]any{"path": "https://evil.example"}, nil, http.StatusBadRequest)
	h.call(http.MethodPost, "/api/v1/rpc/notify.test", map[string]any{"path": "//evil.example"}, nil, http.StatusBadRequest)

	// An unregistered method is a 404 — the registry is the allowlist.
	h.call(http.MethodPost, "/api/v1/rpc/notify.nope", nil, nil, http.StatusNotFound)

	// Each accepted call left the journal row the notify plugin consumes.
	entries, err := h.st.JournalSince(context.Background(), 0, 1000)
	if err != nil {
		t.Fatal(err)
	}
	var paths []string
	for _, e := range entries {
		if e.Kind != "notify.test" {
			continue
		}
		var pl struct {
			Path string `json:"path"`
		}
		if err := json.Unmarshal(e.Payload, &pl); err != nil {
			t.Fatalf("payload %s: %v", e.Payload, err)
		}
		paths = append(paths, pl.Path)
	}
	if len(paths) != 2 || paths[0] != "/attention" || paths[1] != "/tasks/abc" {
		t.Errorf("journaled paths = %v", paths)
	}
}
