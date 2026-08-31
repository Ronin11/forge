package worker

import (
	"context"
	"net/http"
	"sort"
	"time"
)

// runner_probe.go health-probes the configured runners (M10, DESIGN.md §21) and
// advertises each as `runner:<name>` ready|down|unauthenticated in the
// capability map, alongside executor/sandbox/browser. A claude-cli runner
// reuses the executor readiness the worker already computed; an
// openai-compatible runner is probed with a short GET {endpoint}{probe}.

// runnerProbeInterval is how often the worker re-probes runner health (~2 min,
// DESIGN.md §21).
const runnerProbeInterval = 2 * time.Minute

// runnerProbeTimeout bounds a single openai-compatible probe.
const runnerProbeTimeout = 3 * time.Second

// probeRunners sets caps["runner:<name>"] for every configured runner. It is
// called at start (before the first registration) and on the probe loop; the
// capability map is only mutated here and read by registration, so a probe and
// a registration never interleave a half-written state (both run on the worker
// goroutines, serialised by the caller).
func (w *Worker) probeRunners(ctx context.Context) {
	// Snapshot the executor readiness under the lock, probe outside it (openai
	// probes block on the network), then apply the results under capsMu so a
	// concurrent registration always reads a whole map.
	w.capsMu.Lock()
	executors := make(map[string]string, len(w.caps))
	for k, v := range w.caps {
		executors[k] = v
	}
	w.capsMu.Unlock()
	states := make(map[string]string, len(w.cfg.Runners))
	for _, name := range sortedRunnerNames(w.cfg.Runners) {
		states[name] = probeRunner(ctx, w.cfg.Runners[name], executors)
	}
	w.capsMu.Lock()
	for name, state := range states {
		w.caps["runner:"+name] = state
	}
	w.capsMu.Unlock()
}

// probeRunner returns one runner's advertised state; executors is the current
// executor:<name> readiness a claude-cli runner reuses.
func probeRunner(ctx context.Context, rc RunnerConfig, executors map[string]string) string {
	switch rc.Kind {
	case "claude-cli":
		// Reuse the executor readiness: the runner is ready exactly when its
		// executor's command is on PATH (runner.go computed executor:<name>).
		executor := rc.Executor
		if executor == "" {
			executor = "claude-code"
		}
		if executors["executor:"+executor] == "ready" {
			return "ready"
		}
		return "down"
	case "openai-compatible":
		return probeOpenAI(ctx, rc.Endpoint, rc.Probe)
	default:
		return "down"
	}
}

// probeOpenAI does a bounded GET against the models endpoint: 2xx is ready,
// 401/403 is unauthenticated (reachable but no valid key), anything else — a
// non-2xx status or a transport error — is down.
func probeOpenAI(ctx context.Context, endpoint, probe string) string {
	if probe == "" {
		probe = "/v1/models"
	}
	ctx, cancel := context.WithTimeout(ctx, runnerProbeTimeout)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, endpoint+probe, nil)
	if err != nil {
		return "down"
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "down"
	}
	status := resp.StatusCode
	if cerr := resp.Body.Close(); cerr != nil {
		// A probe whose connection cannot even be closed cleanly is not a
		// reachable runner; report it down rather than discard the error.
		return "down"
	}
	switch {
	case status >= 200 && status < 300:
		return "ready"
	case status == http.StatusUnauthorized || status == http.StatusForbidden:
		return "unauthenticated"
	default:
		return "down"
	}
}

// runnerProbeLoop re-probes runner health on the probe interval and re-registers
// so the daemon sees the change promptly.
func (w *Worker) runnerProbeLoop(ctx context.Context) {
	t := time.NewTicker(runnerProbeInterval)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			w.probeRunners(ctx)
			w.register(ctx)
		}
	}
}

func sortedRunnerNames(runners map[string]RunnerConfig) []string {
	out := make([]string, 0, len(runners))
	for name := range runners {
		out = append(out, name)
	}
	sort.Strings(out)
	return out
}
