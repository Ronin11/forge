package main

import (
	"os"
	"testing"

	"forge/internal/plugin"
)

// TestManifestLoads proves plugin.toml is a valid manifest whose name matches
// this directory and whose declared capabilities and scopes are the intake +
// annotate set this plugin relies on. It is the one place the plugin's
// runtime package touches internal/plugin — only to validate the manifest.
func TestManifestLoads(t *testing.T) {
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	m, err := plugin.Load(wd)
	if err != nil {
		t.Fatalf("load manifest: %v", err)
	}
	if m.Name != "github-issues" {
		t.Errorf("name = %q, want github-issues", m.Name)
	}
	if m.Version == "" {
		t.Error("version is empty")
	}
	if m.Restart != "always" {
		t.Errorf("restart = %q, want always", m.Restart)
	}
	if !m.Has(plugin.CapIntake) || !m.Has(plugin.CapAnnotate) {
		t.Errorf("capabilities = %v, want intake + annotate", m.Capabilities)
	}
	for _, s := range []string{plugin.ScopeWorkRead, plugin.ScopeWorkWrite, plugin.ScopeAnnotateWrite} {
		if !m.HasScope(s) {
			t.Errorf("missing scope %q (have %v)", s, m.Scopes)
		}
	}
}
