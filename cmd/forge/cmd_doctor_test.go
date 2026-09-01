package main

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"forge/internal/core/doctor"
)

// fakeBinDir builds a PATH with stub executables so the binary checks never
// depend on the machine running the tests.
func fakeBinDir(t *testing.T, names ...string) string {
	t.Helper()
	dir := t.TempDir()
	for _, name := range names {
		script := "#!/bin/sh\necho " + name + " version 0.0-test\n"
		if err := os.WriteFile(filepath.Join(dir, name), []byte(script), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	return dir
}

func doctorTestContext(t *testing.T, home string) (*cmdContext, *strings.Builder) {
	t.Helper()
	var out strings.Builder
	return &cmdContext{
		stdout: &out, stderr: &out, getenv: os.Getenv,
		forgeHome: home, userHome: t.TempDir(), now: time.Now,
	}, &out
}

func TestDoctorLocal(t *testing.T) {
	t.Setenv("PATH", fakeBinDir(t, "claude", "git", "gh", "node", "npx"))
	home := filepath.Join(t.TempDir(), "forge")
	if err := os.MkdirAll(home, 0o700); err != nil {
		t.Fatal(err)
	}
	token := filepath.Join(home, "token")
	if err := os.WriteFile(token, []byte("t\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	// A world-readable token is the failure that flips the exit code.
	c, out := doctorTestContext(t, home)
	if code := runDoctor(context.Background(), c, nil); code != 1 {
		t.Fatalf("exit = %d, want 1\n%s", code, out.String())
	}
	if !strings.Contains(out.String(), "chmod 600 "+token) {
		t.Errorf("no chmod hint in:\n%s", out.String())
	}
	if !strings.Contains(out.String(), "FAIL  token") {
		t.Errorf("no FAIL row for the token in:\n%s", out.String())
	}

	// Fixed permissions: warns remain (no daemon, no db) but nothing fails.
	if err := os.Chmod(token, 0o600); err != nil {
		t.Fatal(err)
	}
	c, out = doctorTestContext(t, home)
	if code := runDoctor(context.Background(), c, nil); code != 0 {
		t.Fatalf("exit = %d, want 0\n%s", code, out.String())
	}
	if !strings.Contains(out.String(), "daemon not reachable") {
		t.Errorf("no daemon-down warn in:\n%s", out.String())
	}

	// --json emits the merged list.
	c, out = doctorTestContext(t, home)
	if code := runDoctor(context.Background(), c, []string{"--json"}); code != 0 {
		t.Fatalf("--json exit = %d\n%s", code, out.String())
	}
	var checks []doctor.Check
	if err := json.Unmarshal([]byte(out.String()), &checks); err != nil {
		t.Fatalf("decode --json output: %v\n%s", err, out.String())
	}
	names := map[string]bool{}
	for _, ch := range checks {
		names[ch.Name] = true
	}
	for _, want := range []string{"binary.claude", "home", "token", "db", "socket", "lock", "disk", "api"} {
		if !names[want] {
			t.Errorf("missing check %q in %v", want, names)
		}
	}
}

func TestDoctorMissingRequiredBinaryFails(t *testing.T) {
	t.Setenv("PATH", fakeBinDir(t, "git")) // claude absent → fail
	home := filepath.Join(t.TempDir(), "forge")
	if err := os.MkdirAll(home, 0o700); err != nil {
		t.Fatal(err)
	}
	c, out := doctorTestContext(t, home)
	if code := runDoctor(context.Background(), c, nil); code != 1 {
		t.Fatalf("exit = %d, want 1\n%s", code, out.String())
	}
	if !strings.Contains(out.String(), "FAIL  binary.claude") {
		t.Errorf("claude row not failed:\n%s", out.String())
	}
	// gh/node missing must be warns, never part of the failure.
	if strings.Contains(out.String(), "FAIL  binary.gh") || strings.Contains(out.String(), "FAIL  binary.node") {
		t.Errorf("gh/node must warn, not fail:\n%s", out.String())
	}
}
