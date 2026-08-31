package doctor

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestBinaries(t *testing.T) {
	look := func(name string) (string, error) {
		switch name {
		case "claude", "git":
			return "/usr/bin/" + name, nil
		}
		return "", errors.New("not found")
	}
	version := func(name string) string {
		if name == "git" {
			return "git version 2.51.0"
		}
		return ""
	}
	checks := Binaries(look, version)
	byName := map[string]Check{}
	for _, c := range checks {
		byName[c.Name] = c
	}
	if c := byName["binary.claude"]; c.Status != StatusOK || c.Detail != "/usr/bin/claude" {
		t.Errorf("claude = %+v", c)
	}
	if c := byName["binary.git"]; c.Status != StatusOK || !strings.Contains(c.Detail, "git version 2.51.0") {
		t.Errorf("git = %+v", c)
	}
	// gh and node are recommended: missing is a warn, never a fail.
	for _, name := range []string{"binary.gh", "binary.node", "binary.npx"} {
		if c := byName[name]; c.Status != StatusWarn {
			t.Errorf("%s = %+v, want warn", name, c)
		}
	}
	missingAll := Binaries(func(string) (string, error) { return "", errors.New("no") }, func(string) string { return "" })
	for _, c := range missingAll {
		want := StatusWarn
		if c.Name == "binary.claude" || c.Name == "binary.git" {
			want = StatusFail
		}
		if c.Status != want {
			t.Errorf("%s = %s, want %s", c.Name, c.Status, want)
		}
	}
}

func TestHomeAndTokenAndDB(t *testing.T) {
	dir := t.TempDir()
	if err := os.Chmod(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	if c := Home(dir); c.Status != StatusOK {
		t.Errorf("home 0700 = %+v", c)
	}
	if c := Home(filepath.Join(dir, "missing")); c.Status != StatusFail {
		t.Errorf("missing home = %+v", c)
	}
	if err := os.Chmod(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	if c := Home(dir); c.Status != StatusWarn || !strings.Contains(c.Hint, "chmod 700") {
		t.Errorf("home 0755 = %+v", c)
	}

	token := filepath.Join(dir, "token")
	if c := Token(token); c.Status != StatusWarn {
		t.Errorf("missing token = %+v", c)
	}
	if err := os.WriteFile(token, []byte("t\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	// A world-readable token is the one local hard failure with a chmod hint.
	if c := Token(token); c.Status != StatusFail || c.Hint != "chmod 600 "+token {
		t.Errorf("0644 token = %+v", c)
	}
	if err := os.Chmod(token, 0o600); err != nil {
		t.Fatal(err)
	}
	if c := Token(token); c.Status != StatusOK {
		t.Errorf("0600 token = %+v", c)
	}

	db := filepath.Join(dir, "forge.sqlite3")
	if c := DB(db); c.Status != StatusWarn {
		t.Errorf("missing db = %+v", c)
	}
	if err := os.WriteFile(db, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	if c := DB(db); c.Status != StatusOK {
		t.Errorf("db = %+v", c)
	}
	if err := os.Chmod(db, 0o644); err != nil {
		t.Fatal(err)
	}
	if c := DB(db); c.Status != StatusWarn {
		t.Errorf("0644 db = %+v", c)
	}
}

func TestDisk(t *testing.T) {
	if c := Disk(t.TempDir()); c.Status != StatusOK && c.Status != StatusWarn {
		t.Errorf("disk = %+v", c)
	}
}

func TestSocketAndLock(t *testing.T) {
	cases := []struct {
		name                 string
		facts                DaemonFacts
		wantSocket, wantLock string
	}{
		{"daemon up", DaemonFacts{SocketExists: true, LockHeld: true, StateExists: true, PIDAlive: true, PID: 42}, StatusOK, StatusOK},
		{"lock held no socket", DaemonFacts{LockHeld: true}, StatusFail, StatusWarn},
		{"stale socket", DaemonFacts{SocketExists: true}, StatusWarn, StatusOK},
		{"down clean", DaemonFacts{}, StatusOK, StatusOK},
		{"dead pid in state", DaemonFacts{StateExists: true, PID: 7}, StatusOK, StatusWarn},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if c := Socket(tc.facts); c.Status != tc.wantSocket {
				t.Errorf("Socket = %+v, want %s", c, tc.wantSocket)
			}
			if c := Lock(tc.facts); c.Status != tc.wantLock {
				t.Errorf("Lock = %+v, want %s", c, tc.wantLock)
			}
		})
	}
	if c := Socket(DaemonFacts{SocketExists: true}); !strings.Contains(c.Detail, "stale") {
		t.Errorf("stale socket detail = %q", c.Detail)
	}
}

func TestAnyFailed(t *testing.T) {
	if AnyFailed([]Check{{Status: StatusOK}, {Status: StatusWarn}}) {
		t.Error("warn must not count as failed")
	}
	if !AnyFailed([]Check{{Status: StatusOK}, {Status: StatusFail}}) {
		t.Error("fail must count")
	}
}
