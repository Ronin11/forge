package main

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"
)

// TestRegistryUpToDate is the drift check behind just generate-check: the
// committed all/registry_gen.go must byte-equal what generate renders from
// the current directory listing.
func TestRegistryUpToDate(t *testing.T) {
	got, err := generate("..")
	if err != nil {
		t.Fatalf("generate: %v", err)
	}
	want, err := os.ReadFile(filepath.Join("..", "all", "registry_gen.go"))
	if err != nil {
		t.Fatalf("read committed registry: %v", err)
	}
	if !bytes.Equal(got, want) {
		t.Errorf("all/registry_gen.go is stale; run: go generate ./internal/modes")
	}
}
