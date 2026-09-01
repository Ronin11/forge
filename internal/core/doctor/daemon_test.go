package doctor

import (
	"strings"
	"testing"
	"time"

	"forge/internal/core/store"
)

func TestDaemonChecks(t *testing.T) {
	now := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	find := func(checks []Check, name string) Check {
		t.Helper()
		for _, c := range checks {
			if c.Name == name {
				return c
			}
		}
		t.Fatalf("no check %q in %+v", name, checks)
		return Check{}
	}

	// A fresh daemon: no worker, no repos, nothing indexed, no samples.
	fresh := Daemon(DaemonInput{Version: "v1", SchemaVersion: "3", Now: now, StartedAt: now.Add(-time.Hour)})
	if c := find(fresh, "daemon"); c.Status != StatusOK || !strings.Contains(c.Detail, "up 1h") {
		t.Errorf("daemon = %+v", c)
	}
	if c := find(fresh, "schema"); c.Status != StatusOK || c.Detail != "3" {
		t.Errorf("schema = %+v", c)
	}
	for _, name := range []string{"worker", "repositories", "kb", "budget"} {
		if c := find(fresh, name); c.Status != StatusWarn {
			t.Errorf("%s = %+v, want warn", name, c)
		}
	}
	if c := find(fresh, "worktrees"); c.Status != StatusOK {
		t.Errorf("worktrees = %+v", c)
	}

	// A healthy running system, with one degraded capability.
	healthy := Daemon(DaemonInput{
		Version: "v1", SchemaVersion: "3", Now: now,
		Workers: []store.Worker{{
			Name: "local", MaxConcurrent: 4, LastSeenAt: now.Add(-10 * time.Second), Connected: true,
			Capabilities: map[string]string{"executor:claude-code": "ready", "browser": "missing", "sandbox": "ready"},
		}},
		Repositories:    []store.Repository{{Name: "forge"}},
		KbLastIndexedAt: now.Add(-2 * time.Minute),
		RetainedCount:   2,
		FiveHourSample:  &store.RateLimitSample{Window: "five_hour", Utilization: 0.4, Time: now.Add(-time.Minute)},
	})
	if c := find(healthy, "worker.local"); c.Status != StatusOK {
		t.Errorf("worker.local = %+v", c)
	}
	if c := find(healthy, "local.executor:claude-code"); c.Status != StatusOK || c.Detail != "ready" {
		t.Errorf("executor = %+v", c)
	}
	if c := find(healthy, "local.browser"); c.Status != StatusWarn || !strings.Contains(c.Hint, "--with-browser") {
		t.Errorf("browser = %+v", c)
	}
	if c := find(healthy, "repositories"); c.Status != StatusOK || !strings.Contains(c.Detail, "forge") {
		t.Errorf("repositories = %+v", c)
	}
	if c := find(healthy, "kb"); c.Status != StatusOK {
		t.Errorf("kb = %+v", c)
	}
	if c := find(healthy, "worktrees"); c.Status != StatusWarn || !strings.Contains(c.Detail, "2 retained") {
		t.Errorf("worktrees = %+v", c)
	}
	if c := find(healthy, "budget"); c.Status != StatusOK || !strings.Contains(c.Detail, "40%") {
		t.Errorf("budget = %+v", c)
	}

	// Staleness: an old heartbeat, an old index, an old sample.
	stale := Daemon(DaemonInput{
		Version: "v1", SchemaVersion: "3", Now: now,
		Workers:         []store.Worker{{Name: "local", LastSeenAt: now.Add(-10 * time.Minute)}},
		KbLastIndexedAt: now.Add(-time.Hour),
		SevenDaySample:  &store.RateLimitSample{Window: "seven_day", Utilization: 0.8, Time: now.Add(-8 * time.Hour)},
	})
	if c := find(stale, "worker.local"); c.Status != StatusWarn {
		t.Errorf("stale worker = %+v", c)
	}
	if c := find(stale, "kb"); c.Status != StatusWarn {
		t.Errorf("stale kb = %+v", c)
	}
	if c := find(stale, "budget"); c.Status != StatusWarn {
		t.Errorf("stale budget = %+v", c)
	}
}
