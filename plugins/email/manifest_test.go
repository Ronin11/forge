package main

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/BurntSushi/toml"
)

// manifest is the slice of plugin.toml this test asserts — a plugin's own copy
// of the field shape, exactly as an out-of-tree plugin would keep it. The
// contract lives in docs/PLUGINS.md; a plugin consumes it and never imports
// forge's Go packages, so this test validates the manifest by decoding the file
// itself rather than through internal/core/plugin.
type manifest struct {
	Name         string   `toml:"name"`
	Version      string   `toml:"version"`
	Command      []string `toml:"command"`
	Build        []string `toml:"build"`
	Capabilities []string `toml:"capabilities"`
	Scopes       []string `toml:"scopes"`
	Restart      string   `toml:"restart"`
}

func (m manifest) has(list []string, want string) bool {
	for _, s := range list {
		if s == want {
			return true
		}
	}
	return false
}

// TestManifestLoads proves plugin.toml is well formed: its name matches this
// directory (the daemon enforces the same equality) and it declares the events
// + intake capabilities and the scopes both halves rely on.
func TestManifestLoads(t *testing.T) {
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	var m manifest
	if _, err := toml.DecodeFile(filepath.Join(wd, "plugin.toml"), &m); err != nil {
		t.Fatalf("decode plugin.toml: %v", err)
	}
	if m.Name != "email" {
		t.Errorf("name = %q, want email", m.Name)
	}
	if filepath.Base(wd) != m.Name {
		t.Errorf("directory %q must match manifest name %q", filepath.Base(wd), m.Name)
	}
	if m.Version == "" {
		t.Error("version is empty")
	}
	if len(m.Command) == 0 || len(m.Build) == 0 {
		t.Error("command and build must both be set")
	}
	if m.Restart != "always" {
		t.Errorf("restart = %q, want always", m.Restart)
	}
	for _, c := range []string{"events", "intake"} {
		if !m.has(m.Capabilities, c) {
			t.Errorf("capabilities = %v, want to include %q", m.Capabilities, c)
		}
	}
	for _, s := range []string{"events:read", "work:read", "work:write"} {
		if !m.has(m.Scopes, s) {
			t.Errorf("missing scope %q (have %v)", s, m.Scopes)
		}
	}
}
