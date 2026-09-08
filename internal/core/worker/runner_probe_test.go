package worker

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
)

// TestProbeOpenAIDefaultPath pins the probe URL an openai-compatible runner is
// health-checked on. The endpoint carries the API version (…/v1, as the config
// documents and the config tests use), so the default probe path must not
// repeat it: a /v1/models default built …/v1/v1/models and reported every
// healthy runner down.
func TestProbeOpenAIDefaultPath(t *testing.T) {
	var got string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		got = r.URL.Path
		if r.URL.Path != "/v1/models" {
			w.WriteHeader(http.StatusNotFound)
			return
		}
		w.WriteHeader(http.StatusOK)
	}))
	defer srv.Close()

	if state := probeOpenAI(context.Background(), srv.URL+"/v1", ""); state != "ready" {
		t.Errorf("probeOpenAI(endpoint+/v1, default) = %q, want ready (probed %q)", state, got)
	}
	if got != "/v1/models" {
		t.Errorf("probed path = %q, want /v1/models", got)
	}
}

// TestProbeOpenAIStates maps transport and status outcomes onto the three
// advertised states.
func TestProbeOpenAIStates(t *testing.T) {
	for _, tc := range []struct {
		name   string
		status int
		want   string
	}{
		{"ok", http.StatusOK, "ready"},
		{"unauthorized", http.StatusUnauthorized, "unauthenticated"},
		{"forbidden", http.StatusForbidden, "unauthenticated"},
		{"server error", http.StatusInternalServerError, "down"},
		{"not found", http.StatusNotFound, "down"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				w.WriteHeader(tc.status)
			}))
			defer srv.Close()
			if state := probeOpenAI(context.Background(), srv.URL, "/models"); state != tc.want {
				t.Errorf("status %d = %q, want %q", tc.status, state, tc.want)
			}
		})
	}
}

// TestProbeOpenAIUnreachable is the transport-error case: nothing listening.
func TestProbeOpenAIUnreachable(t *testing.T) {
	// Port 9 (discard) refuses on the loopback; the same address route_claim's
	// tests use for an endpoint that is configured but never up.
	if state := probeOpenAI(context.Background(), "http://127.0.0.1:9/v1", ""); state != "down" {
		t.Errorf("unreachable endpoint = %q, want down", state)
	}
}

// TestProbeOpenAIExplicitProbeOverrides checks a configured probe path is used
// verbatim, so an endpoint that serves its model list elsewhere still works.
func TestProbeOpenAIExplicitProbeOverrides(t *testing.T) {
	var got string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		got = r.URL.Path
		w.WriteHeader(http.StatusOK)
	}))
	defer srv.Close()

	if state := probeOpenAI(context.Background(), srv.URL, "/healthz"); state != "ready" {
		t.Errorf("explicit probe = %q, want ready", state)
	}
	if got != "/healthz" {
		t.Errorf("probed path = %q, want /healthz", got)
	}
}

// TestProbeRunnerClaudeCLIReusesExecutor covers the other runner kind: a
// claude-cli runner is ready exactly when its executor is.
func TestProbeRunnerClaudeCLIReusesExecutor(t *testing.T) {
	for _, tc := range []struct {
		name string
		cfg  RunnerConfig
		caps map[string]string
		want string
	}{
		{
			name: "default executor ready",
			cfg:  RunnerConfig{Kind: "claude-cli"},
			caps: map[string]string{"executor:claude-code": "ready"},
			want: "ready",
		},
		{
			name: "default executor missing",
			cfg:  RunnerConfig{Kind: "claude-cli"},
			caps: map[string]string{"executor:claude-code": "missing"},
			want: "down",
		},
		{
			name: "named executor ready",
			cfg:  RunnerConfig{Kind: "claude-cli", Executor: "other"},
			caps: map[string]string{"executor:other": "ready", "executor:claude-code": "missing"},
			want: "ready",
		},
		{
			name: "unknown kind",
			cfg:  RunnerConfig{Kind: "wat"},
			caps: map[string]string{},
			want: "down",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if state := probeRunner(context.Background(), tc.cfg, tc.caps); state != tc.want {
				t.Errorf("probeRunner = %q, want %q", state, tc.want)
			}
		})
	}
}
