package controlplane

import (
	"os"
	"path/filepath"
	"testing"
)

// plugin_dirs entries are resolved at load: ~ expands to the user's home, a
// relative path resolves against config.toml's own directory, and an absolute
// path is left as is.
func TestPluginDirsResolution(t *testing.T) {
	home := t.TempDir()     // config lives here; also the relative base
	userHome := t.TempDir() // distinct so ~ expansion is observable
	path := filepath.Join(home, "config.toml")
	body := "plugin_dirs = [\"~/forge-plugins\", \"extra/plugins\", \"/opt/forge/plugins\"]\n"
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	c, err := LoadConfig(path, home, userHome, func(string) string { return "" })
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	want := []string{
		filepath.Join(userHome, "forge-plugins"),
		filepath.Join(home, "extra", "plugins"),
		"/opt/forge/plugins",
	}
	if len(c.PluginDirs) != len(want) {
		t.Fatalf("plugin_dirs = %v, want %v", c.PluginDirs, want)
	}
	for i := range want {
		if c.PluginDirs[i] != want[i] {
			t.Errorf("plugin_dirs[%d] = %q, want %q", i, c.PluginDirs[i], want[i])
		}
	}
}

// PluginRoots lists <home>/plugins first (so a locally installed plugin
// shadows a configured one), then each configured dir; a missing or unreadable
// configured root warns but is still listed and never fails.
func TestPluginRootsWarnsOnMissing(t *testing.T) {
	home := t.TempDir()
	present := t.TempDir()
	missing := filepath.Join(t.TempDir(), "gone")
	c := &Config{PluginDirs: []string{present, missing}}

	var warned []string
	roots := c.PluginRoots(home, func(dir string, err error) {
		warned = append(warned, dir)
	})
	want := []string{filepath.Join(home, "plugins"), present, missing}
	if len(roots) != len(want) {
		t.Fatalf("roots = %v, want %v", roots, want)
	}
	for i := range want {
		if roots[i] != want[i] {
			t.Errorf("roots[%d] = %q, want %q", i, roots[i], want[i])
		}
	}
	if len(warned) != 1 || warned[0] != missing {
		t.Errorf("warned = %v, want just the missing root %q", warned, missing)
	}
}

// A config with no plugin_dirs yields exactly the built-in root.
func TestPluginRootsDefault(t *testing.T) {
	home := t.TempDir()
	c := &Config{}
	roots := c.PluginRoots(home, func(dir string, err error) {
		t.Errorf("unexpected warn: %s: %v", dir, err)
	})
	if len(roots) != 1 || roots[0] != filepath.Join(home, "plugins") {
		t.Fatalf("roots = %v, want just <home>/plugins", roots)
	}
}
