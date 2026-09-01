package plugin

import (
	"os"
	"path/filepath"
	"testing"
)

// writePlugin creates root/name/plugin.toml for a minimal valid events plugin.
func writePlugin(t *testing.T, root, name, version string) {
	t.Helper()
	dir := filepath.Join(root, name)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	body := "name = \"" + name + "\"\n" +
		"version = \"" + version + "\"\n" +
		"command = [\"./" + name + "\"]\n" +
		"capabilities = [\"events\"]\n" +
		"scopes = [\"events:read\"]\n"
	if err := os.WriteFile(filepath.Join(dir, "plugin.toml"), []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
}

// Discovery spans every root and returns manifests sorted by name.
func TestDiscoverAcrossRoots(t *testing.T) {
	a, b := t.TempDir(), t.TempDir()
	writePlugin(t, a, "alpha", "1.0.0")
	writePlugin(t, b, "beta", "2.0.0")

	got := Discover([]string{a, b}, func(dir string, err error) {
		t.Errorf("unexpected onErr: %s: %v", dir, err)
	})
	if len(got) != 2 || got[0].Name != "alpha" || got[1].Name != "beta" {
		t.Fatalf("discover = %v, want alpha then beta", names(got))
	}
	if got[0].Dir != filepath.Join(a, "alpha") || got[1].Dir != filepath.Join(b, "beta") {
		t.Errorf("dirs = %q, %q", got[0].Dir, got[1].Dir)
	}
}

// A name in two roots resolves to the earlier root; the shadowed copy is
// reported (a warning at the daemon), never fatal.
func TestDiscoverEarlierRootWins(t *testing.T) {
	early, late := t.TempDir(), t.TempDir()
	writePlugin(t, early, "dup", "1.0.0")
	writePlugin(t, late, "dup", "9.9.9")

	var shadowed []string
	got := Discover([]string{early, late}, func(dir string, err error) {
		shadowed = append(shadowed, dir)
	})
	if len(got) != 1 || got[0].Version != "1.0.0" {
		t.Fatalf("discover = %v, want the early root's 1.0.0", got)
	}
	if got[0].Dir != filepath.Join(early, "dup") {
		t.Errorf("winner dir = %q, want the early root", got[0].Dir)
	}
	if len(shadowed) != 1 || shadowed[0] != filepath.Join(late, "dup") {
		t.Errorf("shadowed reported = %v, want the late root's copy", shadowed)
	}
}

// A missing root is skipped without a fatal error; plugins in the roots that do
// exist still come back.
func TestDiscoverMissingRootSkipped(t *testing.T) {
	present := t.TempDir()
	writePlugin(t, present, "alpha", "1.0.0")
	missing := filepath.Join(t.TempDir(), "does-not-exist")

	got := Discover([]string{missing, present}, func(dir string, err error) {
		t.Errorf("unexpected onErr for missing root: %s: %v", dir, err)
	})
	if len(got) != 1 || got[0].Name != "alpha" {
		t.Fatalf("discover = %v, want just alpha", names(got))
	}
}

// LoadFromRoots resolves a name across roots in the same earlier-wins order.
func TestLoadFromRoots(t *testing.T) {
	early, late := t.TempDir(), t.TempDir()
	writePlugin(t, late, "solo", "3.0.0")
	writePlugin(t, early, "dup", "1.0.0")
	writePlugin(t, late, "dup", "2.0.0")

	m, err := LoadFromRoots([]string{early, late}, "solo")
	if err != nil {
		t.Fatalf("load solo: %v", err)
	}
	if m.Version != "3.0.0" {
		t.Errorf("solo version = %q", m.Version)
	}
	m, err = LoadFromRoots([]string{early, late}, "dup")
	if err != nil {
		t.Fatalf("load dup: %v", err)
	}
	if m.Version != "1.0.0" {
		t.Errorf("dup resolved to %q, want the early root's 1.0.0", m.Version)
	}
	if _, err := LoadFromRoots([]string{early, late}, "absent"); err == nil {
		t.Error("expected an error for a name in no root")
	}
}

func names(ms []*Manifest) []string {
	out := make([]string, len(ms))
	for i, m := range ms {
		out[i] = m.Name
	}
	return out
}
