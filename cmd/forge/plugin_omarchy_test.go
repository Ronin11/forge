package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"forge/internal/plugin"
)

// The repo's plugin.toml must satisfy the manifest contract, and the source
// locator must find it from this package's working directory.
func TestOmarchyPluginManifestLoads(t *testing.T) {
	dir, err := omarchyIndicatorSource()
	if err != nil {
		t.Fatal(err)
	}
	m, err := plugin.Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	if m.Name != "omarchy-indicator" {
		t.Fatalf("name = %q, want omarchy-indicator", m.Name)
	}
	if m.Restart != "never" {
		t.Fatalf("restart = %q, want never", m.Restart)
	}
}

// fixtureShellJSON mirrors the shape the shell writes: objects with ids,
// two-space indentation, and omarchy.agents in the right section.
const fixtureShellJSON = `{
  "bar": {
    "layout": {
      "center": [
        { "id": "omarchy.clock" }
      ],
      "left": [
        { "id": "omarchy.menu" }
      ],
      "right": [
        { "id": "omarchy.tray" },
        { "id": "omarchy.agents" },
        { "id": "omarchy.audio" }
      ]
    },
    "position": "top"
  },
  "version": 1
}
`

func writeShellFixture(t *testing.T, content string) (path, backup string) {
	t.Helper()
	dir := t.TempDir()
	path = filepath.Join(dir, "shell.json")
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	return path, path + ".forge-backup-1"
}

func rightSectionIDs(t *testing.T, path string) []string {
	t.Helper()
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var doc map[string]any
	if err := json.Unmarshal(raw, &doc); err != nil {
		t.Fatalf("parse %s: %v", path, err)
	}
	layout := shellBarLayout(doc, false)
	var ids []string
	for _, e := range shellLayoutSection(layout, "right") {
		ids = append(ids, shellEntryID(e))
	}
	return ids
}

func TestOmarchyAddEntryInsertsBeforeAgents(t *testing.T) {
	path, backup := writeShellFixture(t, fixtureShellJSON)
	changed, err := addOmarchyBarEntry(path, backup)
	if err != nil {
		t.Fatal(err)
	}
	if !changed {
		t.Fatal("addOmarchyBarEntry reported no change")
	}
	got := rightSectionIDs(t, path)
	want := []string{"omarchy.tray", "ronin.forge", "omarchy.agents", "omarchy.audio"}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("right section = %v, want %v", got, want)
	}
}

func TestOmarchyAddEntryAppendsWhenAgentsAbsent(t *testing.T) {
	fixture := strings.Replace(fixtureShellJSON, "{ \"id\": \"omarchy.agents\" },\n        ", "", 1)
	path, backup := writeShellFixture(t, fixture)
	changed, err := addOmarchyBarEntry(path, backup)
	if err != nil {
		t.Fatal(err)
	}
	if !changed {
		t.Fatal("addOmarchyBarEntry reported no change")
	}
	got := rightSectionIDs(t, path)
	want := []string{"omarchy.tray", "omarchy.audio", "ronin.forge"}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("right section = %v, want %v", got, want)
	}
}

func TestOmarchyAddEntryIdempotent(t *testing.T) {
	path, backup := writeShellFixture(t, fixtureShellJSON)
	if _, err := addOmarchyBarEntry(path, backup); err != nil {
		t.Fatal(err)
	}
	before, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	secondBackup := path + ".forge-backup-2"
	changed, err := addOmarchyBarEntry(path, secondBackup)
	if err != nil {
		t.Fatal(err)
	}
	if changed {
		t.Fatal("second addOmarchyBarEntry reported a change")
	}
	after, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if string(before) != string(after) {
		t.Fatal("second addOmarchyBarEntry rewrote the file")
	}
	if _, err := os.Stat(secondBackup); !os.IsNotExist(err) {
		t.Fatal("no-op edit wrote a backup")
	}
}

// The shell also accepts bare-string layout entries; the id match must too.
func TestOmarchyAddEntrySeesStringEntries(t *testing.T) {
	fixture := `{"bar":{"layout":{"right":["omarchy.tray","ronin.forge","omarchy.agents"]}}}`
	path, backup := writeShellFixture(t, fixture)
	changed, err := addOmarchyBarEntry(path, backup)
	if err != nil {
		t.Fatal(err)
	}
	if changed {
		t.Fatal("addOmarchyBarEntry duplicated a string entry")
	}
}

func TestOmarchyAddEntryBackupHoldsOriginal(t *testing.T) {
	path, backup := writeShellFixture(t, fixtureShellJSON)
	if _, err := addOmarchyBarEntry(path, backup); err != nil {
		t.Fatal(err)
	}
	saved, err := os.ReadFile(backup)
	if err != nil {
		t.Fatalf("backup missing: %v", err)
	}
	if string(saved) != fixtureShellJSON {
		t.Fatal("backup does not hold the original content")
	}
}

func TestOmarchyRemoveEntry(t *testing.T) {
	path, backup := writeShellFixture(t, fixtureShellJSON)
	if _, err := addOmarchyBarEntry(path, backup); err != nil {
		t.Fatal(err)
	}
	removeBackup := path + ".forge-backup-2"
	changed, err := removeOmarchyBarEntry(path, removeBackup)
	if err != nil {
		t.Fatal(err)
	}
	if !changed {
		t.Fatal("removeOmarchyBarEntry reported no change")
	}
	got := rightSectionIDs(t, path)
	want := []string{"omarchy.tray", "omarchy.agents", "omarchy.audio"}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("right section = %v, want %v", got, want)
	}
	if _, err := os.Stat(removeBackup); err != nil {
		t.Fatalf("remove backup missing: %v", err)
	}

	// Removing again is a no-op.
	changed, err = removeOmarchyBarEntry(path, path+".forge-backup-3")
	if err != nil {
		t.Fatal(err)
	}
	if changed {
		t.Fatal("second removeOmarchyBarEntry reported a change")
	}
}

func TestOmarchyHasEntryMissingFile(t *testing.T) {
	present, err := shellJSONHasEntry(filepath.Join(t.TempDir(), "shell.json"), omarchyPluginID)
	if err != nil {
		t.Fatal(err)
	}
	if present {
		t.Fatal("missing file reported an entry")
	}
}

func TestOmarchyCopyPluginFiles(t *testing.T) {
	src := t.TempDir()
	dst := filepath.Join(t.TempDir(), "ronin.forge")
	files := map[string]string{
		"manifest.json": `{"id":"ronin.forge"}`,
		"Panel.qml":     "import QtQuick\n",
		"Status.qml":    "import QtQuick\n",
		"README.md":     "# omarchy-indicator\n",
		"plugin.toml":   `name = "omarchy-indicator"`,
	}
	for name, content := range files {
		if err := os.WriteFile(filepath.Join(src, name), []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	copied, err := copyOmarchyPluginFiles(src, dst)
	if err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"manifest.json", "Panel.qml", "Status.qml", "README.md"} {
		data, err := os.ReadFile(filepath.Join(dst, name))
		if err != nil {
			t.Fatalf("%s not copied: %v", name, err)
		}
		if string(data) != files[name] {
			t.Fatalf("%s content differs", name)
		}
	}
	if _, err := os.Stat(filepath.Join(dst, "plugin.toml")); !os.IsNotExist(err) {
		t.Fatal("plugin.toml was copied; it belongs to Forge, not Omarchy")
	}
	if len(copied) != 4 {
		t.Fatalf("copied %v, want 4 files", copied)
	}

	// Reinstall overwrites in place.
	if err := os.WriteFile(filepath.Join(src, "Panel.qml"), []byte("// v2\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := copyOmarchyPluginFiles(src, dst); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(filepath.Join(dst, "Panel.qml"))
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != "// v2\n" {
		t.Fatal("reinstall did not overwrite Panel.qml")
	}
}
