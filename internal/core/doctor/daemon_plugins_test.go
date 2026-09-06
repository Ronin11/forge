package doctor

import (
	"strings"
	"testing"

	"forge/internal/core/plugin"
	"forge/internal/core/store"
)

func TestPluginChecks(t *testing.T) {
	t.Parallel()
	in := DaemonInput{
		Plugins: []store.Plugin{
			{Name: "up", Enabled: true},
			{Name: "down", Enabled: true},
			{Name: "off", Enabled: false},
		},
		PluginHealth: []plugin.PluginHealth{
			{Name: "up", Running: true, PID: 42, Restarts: 1},
			{Name: "down", Running: false, Restarts: 5, LastExit: "exit status 1"},
		},
	}
	checks := plugins(in)
	if len(checks) != 2 {
		t.Fatalf("got %d checks, want 2 (disabled plugins are skipped): %+v", len(checks), checks)
	}
	byName := map[string]Check{}
	for _, c := range checks {
		byName[c.Name] = c
	}
	up := byName["plugin.up"]
	if up.Status != StatusOK || !strings.Contains(up.Detail, "pid 42") {
		t.Errorf("plugin.up = %+v, want ok with pid", up)
	}
	down := byName["plugin.down"]
	if down.Status != StatusFail {
		t.Errorf("plugin.down status = %q, want fail", down.Status)
	}
	if !strings.Contains(down.Detail, "exit status 1") || !strings.Contains(down.Detail, "5 restarts") {
		t.Errorf("plugin.down detail = %q, want last exit and restarts", down.Detail)
	}
	if !strings.Contains(down.Hint, "forge plugin logs down") {
		t.Errorf("plugin.down hint = %q, want the logs command", down.Hint)
	}
}

// A sandbox:missing worker is a doctor failure: require_sandbox routines
// (default true) cannot route to it (M8 smoke 6).
func TestCapabilitySandbox(t *testing.T) {
	w := store.Worker{Name: "local", Capabilities: map[string]string{"sandbox": "missing"}}
	var found *Check
	for _, c := range capabilityChecks(w) {
		if c.Name == "local.sandbox" {
			cc := c
			found = &cc
		}
	}
	if found == nil || found.Status != StatusFail || found.Hint == "" {
		t.Fatalf("sandbox check = %+v", found)
	}
	w.Capabilities["sandbox"] = "ready"
	for _, c := range capabilityChecks(w) {
		if c.Name == "local.sandbox" && c.Status != StatusOK {
			t.Errorf("ready sandbox = %+v", c)
		}
	}
}

func TestPluginDenialsCheck(t *testing.T) {
	t.Parallel()
	checks := pluginDenialsCheck([]PluginDenial{
		{Plugin: "signal", Count: 3, LastPath: "/api/v1/assistant/message"},
		{Plugin: "quiet", Count: 0},
	})
	if len(checks) != 1 {
		t.Fatalf("got %d checks, want 1 (zero-count rows are skipped): %+v", len(checks), checks)
	}
	c := checks[0]
	if c.Name != "plugin_denied:signal" || c.Status != "warn" {
		t.Fatalf("check = %+v", c)
	}
	if !strings.Contains(c.Detail, "3 scope denial(s)") || !strings.Contains(c.Detail, "/api/v1/assistant/message") {
		t.Fatalf("detail lacks tally or path: %s", c.Detail)
	}
}
