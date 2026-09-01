package tui

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// gitRepo creates a real checkout under dir, with an origin when url is set.
func gitRepo(t *testing.T, dir, url string) {
	t.Helper()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if out, err := exec.Command("git", "init", "-q", dir).CombinedOutput(); err != nil {
		t.Fatalf("git init: %v: %s", err, out)
	}
	if url != "" {
		if out, err := exec.Command("git", "-C", dir, "remote", "add", "origin", url).CombinedOutput(); err != nil {
			t.Fatalf("git remote add: %v: %s", err, out)
		}
	}
}

// The --yes path takes every default: it reports the plan, registers nothing,
// prompts for nothing, and runs neither npm nor systemctl.
func TestInitYes(t *testing.T) {
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not available")
	}
	userHome := t.TempDir()
	projects := filepath.Join(userHome, "Projects")
	gitRepo(t, filepath.Join(projects, "app1"), "https://example.com/app1.git")
	gitRepo(t, filepath.Join(projects, "no-origin"), "") // no origin → not offered
	if err := os.MkdirAll(filepath.Join(projects, "not-a-repo"), 0o755); err != nil {
		t.Fatal(err)
	}

	home := filepath.Join(t.TempDir(), "forge")
	if err := os.MkdirAll(home, 0o700); err != nil {
		t.Fatal(err)
	}
	workerToml := filepath.Join(home, "worker.toml")
	original := "[repositories.existing]\npath = \"/tmp/existing\"\n"
	if err := os.WriteFile(workerToml, []byte(original), 0o600); err != nil {
		t.Fatal(err)
	}

	var out strings.Builder
	c := &Context{
		Stdout: &out, Stderr: &out,
		Getenv:    func(string) string { return "" },
		ForgeHome: home, UserHome: userHome, Now: time.Now,
	}
	if code := RunInit(context.Background(), c, []string{"--yes"}); code != 0 {
		t.Fatalf("init --yes = %d\n%s", code, out.String())
	}
	got := out.String()
	for _, want := range []string{
		"would register app1",
		"worker.toml left unchanged",
		"(kept; --yes)",
		"service: skipped",
		"browser: skipped",
	} {
		if !strings.Contains(got, want) {
			t.Errorf("output missing %q:\n%s", want, got)
		}
	}
	if strings.Contains(got, "no-origin") || strings.Contains(got, "not-a-repo") {
		t.Errorf("non-candidates offered:\n%s", got)
	}
	after, err := os.ReadFile(workerToml)
	if err != nil {
		t.Fatal(err)
	}
	if string(after) != original {
		t.Errorf("worker.toml changed by --yes:\n%s", after)
	}
}

// An already-registered checkout is counted, not offered again.
func TestInitYesSkipsRegistered(t *testing.T) {
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not available")
	}
	userHome := t.TempDir()
	gitRepo(t, filepath.Join(userHome, "Projects", "app1"), "https://example.com/app1.git")
	home := filepath.Join(t.TempDir(), "forge")
	if err := os.MkdirAll(home, 0o700); err != nil {
		t.Fatal(err)
	}
	reg := "[repositories.app1]\npath = \"" + filepath.Join(userHome, "Projects", "app1") + "\"\n"
	if err := os.WriteFile(filepath.Join(home, "worker.toml"), []byte(reg), 0o600); err != nil {
		t.Fatal(err)
	}
	var out strings.Builder
	c := &Context{
		Stdout: &out, Stderr: &out,
		Getenv:    func(string) string { return "" },
		ForgeHome: home, UserHome: userHome, Now: time.Now,
	}
	if code := RunInit(context.Background(), c, []string{"--yes"}); code != 0 {
		t.Fatalf("init --yes = %d\n%s", code, out.String())
	}
	if !strings.Contains(out.String(), "nothing new to register") {
		t.Errorf("registered repo offered again:\n%s", out.String())
	}
}
