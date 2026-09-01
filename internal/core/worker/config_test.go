package worker

import (
	"path/filepath"
	"testing"
)

// AddRepository appends on-the-fly registrations (DESIGN §1.3) that a
// LoadConfig round-trip then sees — the worker's refresh tick contract.
func TestAddRepository(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "worker.toml")
	if _, err := WriteDefault(path, dir, "/bin/true"); err != nil {
		t.Fatal(err)
	}
	repo := t.TempDir()
	if err := AddRepository(path, "myrepo", repo, "main"); err != nil {
		t.Fatal(err)
	}
	cfg, err := LoadConfig(path)
	if err != nil {
		t.Fatal(err)
	}
	if got := cfg.Repositories["myrepo"].Path; got != repo {
		t.Errorf("path = %q, want %q", got, repo)
	}
	if got := cfg.Repositories["myrepo"].BaseBranch; got != "main" {
		t.Errorf("base_branch = %q, want main", got)
	}
	if err := AddRepository(path, "myrepo", repo, ""); err == nil {
		t.Error("duplicate accepted")
	}
	if err := AddRepository(path, "bad name!", repo, ""); err == nil {
		t.Error("invalid name accepted")
	}
}
