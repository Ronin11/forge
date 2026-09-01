package web

import (
	"net/http"
	"testing"

	"forge/internal/core/store"
)

// The repository detail read, pause/resume, cancel-running, and app-url form
// one operator surface; this drives them end to end against the test harness.
func TestRepositoryControls(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	h.run("inventory")

	// Unknown repository 404s.
	if status, _ := h.do(http.MethodGet, "/api/v1/repositories/ghost", nil, nil, ""); status != http.StatusNotFound {
		t.Fatalf("unknown repo detail = %d, want 404", status)
	}

	// Detail: a pending target is not active, so the repository is idle.
	var d repoDetail
	h.call(http.MethodGet, "/api/v1/repositories/equitizr", nil, &d, http.StatusOK)
	if d.Repository.Name != "equitizr" || d.State != "idle" || d.Paused {
		t.Fatalf("detail = %+v", d)
	}

	// Pause: the repo flips to paused, and the detail state follows.
	var repo store.Repository
	h.call(http.MethodPost, "/api/v1/repositories/equitizr/pause", nil, &repo, http.StatusOK)
	if !repo.Paused {
		t.Fatalf("pause response = %+v", repo)
	}
	h.call(http.MethodGet, "/api/v1/repositories/equitizr", nil, &d, http.StatusOK)
	if !d.Paused || d.State != "paused" {
		t.Fatalf("after pause: %+v", d)
	}

	// A paused repository admits no claim (repository_paused): 204.
	if status, _ := h.claim(testWorkerID, "paused-claim"); status != http.StatusNoContent {
		t.Fatalf("claim on paused repo = %d, want 204", status)
	}

	// Resume, then a claim admits the target and the repo reads running.
	h.call(http.MethodPost, "/api/v1/repositories/equitizr/resume", nil, &repo, http.StatusOK)
	if repo.Paused {
		t.Fatalf("resume response = %+v", repo)
	}
	h.mustClaim("c1")
	h.call(http.MethodGet, "/api/v1/repositories/equitizr", nil, &d, http.StatusOK)
	if d.State != "running" || len(d.Running) != 1 {
		t.Fatalf("after claim: state=%q running=%d", d.State, len(d.Running))
	}

	// cancel-running cancels the one active task.
	var cancelled struct {
		Cancelled int `json:"cancelled"`
	}
	h.call(http.MethodPost, "/api/v1/repositories/equitizr/cancel-running", nil, &cancelled, http.StatusOK)
	if cancelled.Cancelled != 1 {
		t.Fatalf("cancel-running = %+v", cancelled)
	}
}

// The app-url setter rejects a non-http(s) value and stores a valid one, which
// the detail read reflects.
func TestRepositoryAppURL(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)

	if status, _ := h.do(http.MethodPost, "/api/v1/repositories/equitizr/app-url", map[string]string{"url": "ftp://nope"}, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("non-http app url = %d, want 400", status)
	}
	var repo store.Repository
	h.call(http.MethodPost, "/api/v1/repositories/equitizr/app-url", map[string]string{"url": "https://localhost:3000"}, &repo, http.StatusOK)
	if repo.AppURL != "https://localhost:3000" {
		t.Fatalf("app url = %q", repo.AppURL)
	}
	var d repoDetail
	h.call(http.MethodGet, "/api/v1/repositories/equitizr", nil, &d, http.StatusOK)
	if d.AppURL != "https://localhost:3000" {
		t.Fatalf("detail app url = %q", d.AppURL)
	}
	// Empty clears it (the detail read confirms; the response omits an empty
	// app_url by omitempty, so re-read rather than trust a reused struct).
	h.call(http.MethodPost, "/api/v1/repositories/equitizr/app-url", map[string]string{"url": ""}, nil, http.StatusOK)
	var after repoDetail
	h.call(http.MethodGet, "/api/v1/repositories/equitizr", nil, &after, http.StatusOK)
	if after.AppURL != "" {
		t.Fatalf("clear app url = %q", after.AppURL)
	}
}
