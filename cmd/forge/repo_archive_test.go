package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestRepoNameFromURL(t *testing.T) {
	for url, want := range map[string]string{
		"https://github.com/x/y.git":          "y",
		"https://github.com/x/y":              "y",
		"git@github.com:Ronin11/equitizr.git": "equitizr",
		"file:///tmp/xrepo.git":               "xrepo",
		"https://h/a/b/":                      "b",
		"ssh://git@h/org/repo.git":            "repo",
	} {
		if got := repoNameFromURL(url); got != want {
			t.Errorf("repoNameFromURL(%q) = %q, want %q", url, got, want)
		}
	}
}

func TestSafeRemoveCheckout(t *testing.T) {
	// A real git checkout is removed.
	dir := t.TempDir()
	repo := filepath.Join(dir, "repo")
	if err := os.MkdirAll(filepath.Join(repo, ".git"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := safeRemoveCheckout(repo); err != nil {
		t.Fatalf("remove real checkout: %v", err)
	}
	if _, err := os.Stat(repo); !os.IsNotExist(err) {
		t.Fatalf("checkout not removed")
	}

	// Refusals: non-absolute, a non-git directory, a symlink, and the root.
	if err := safeRemoveCheckout("relative/path"); err == nil {
		t.Error("non-absolute path should be refused")
	}
	plain := filepath.Join(dir, "plain")
	if err := os.MkdirAll(plain, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := safeRemoveCheckout(plain); err == nil {
		t.Error("non-git directory should be refused")
	}
	if _, err := os.Stat(plain); err != nil {
		t.Error("refused directory must not be deleted")
	}
	link := filepath.Join(dir, "link")
	target := filepath.Join(dir, "target")
	if err := os.MkdirAll(filepath.Join(target, ".git"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(target, link); err == nil {
		if err := safeRemoveCheckout(link); err == nil {
			t.Error("symlinked checkout should be refused")
		}
	}
	if err := safeRemoveCheckout("/"); err == nil {
		t.Error("root should be refused")
	}
	// A checkout that is already gone is a no-op, not an error (so a repo whose
	// checkout was deleted by hand can still be archived).
	if err := safeRemoveCheckout(filepath.Join(dir, "vanished")); err != nil {
		t.Errorf("missing checkout should be a no-op: %v", err)
	}
}
