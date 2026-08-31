package main

import (
	"log/slog"
	"os"
	"path/filepath"
	"testing"
)

func discardLogger() *slog.Logger { return slog.New(slog.DiscardHandler) }

func TestStateRoundTrip(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "state.json")

	s := loadState(path, discardLogger())
	key := issueKey("Ronin11/forge", 12)
	entry := tracked{WorkID: "abc123def456", Forge: "forge", Number: 12, GitHub: "Ronin11/forge", Status: statusIngested}
	if err := s.put(key, entry); err != nil {
		t.Fatal(err)
	}

	// The file exists at 0600 after the atomic write.
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o600 {
		t.Errorf("state file mode = %v, want 0600", info.Mode().Perm())
	}

	// Reload simulates a restart: the entry is deduped back into memory.
	s2 := loadState(path, discardLogger())
	if !s2.has(key) {
		t.Fatalf("reloaded state missing key %q", key)
	}
	got := s2.entries[key]
	if got.WorkID != "abc123def456" || got.Number != 12 || got.Status != statusIngested {
		t.Errorf("reloaded entry = %+v", got)
	}
}

func TestStateAbsentStartsEmpty(t *testing.T) {
	s := loadState(filepath.Join(t.TempDir(), "nope.json"), discardLogger())
	if len(s.entries) != 0 {
		t.Errorf("absent state should be empty, got %d", len(s.entries))
	}
}

func TestStateMalformedStartsEmpty(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "state.json")
	if err := os.WriteFile(path, []byte("{not json"), 0o600); err != nil {
		t.Fatal(err)
	}
	s := loadState(path, discardLogger())
	if len(s.entries) != 0 {
		t.Errorf("malformed state should start empty, got %d", len(s.entries))
	}
	// And it remains usable — a later put overwrites the garbage cleanly.
	if err := s.put(issueKey("o/n", 1), tracked{WorkID: "w", Number: 1, GitHub: "o/n", Status: statusIngested}); err != nil {
		t.Fatal(err)
	}
	if !loadState(path, discardLogger()).has(issueKey("o/n", 1)) {
		t.Error("state not usable after malformed load")
	}
}

func TestIssueKey(t *testing.T) {
	if got := issueKey("Ronin11/forge", 42); got != "Ronin11/forge#42" {
		t.Errorf("issueKey = %q", got)
	}
}
