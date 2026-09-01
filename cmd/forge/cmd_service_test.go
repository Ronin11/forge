package main

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// stubRunner records systemctl/loginctl invocations instead of running them;
// no test ever touches the live user units.
type stubRunner struct {
	calls  []string
	linger string // what loginctl show-user answers
}

func (s *stubRunner) run(_ context.Context, name string, args ...string) (string, error) {
	s.calls = append(s.calls, name+" "+strings.Join(args, " "))
	if name == "loginctl" {
		return s.linger, nil
	}
	return "", nil
}

func serviceTestContext(t *testing.T, env map[string]string) (*cmdContext, *strings.Builder) {
	t.Helper()
	var out strings.Builder
	c := &cmdContext{
		stdout: &out, stderr: &out,
		getenv:    func(k string) string { return env[k] },
		forgeHome: "/home/tester/.forge", userHome: "/home/tester", now: time.Now,
	}
	return c, &out
}

func TestServiceInstallWritesUnits(t *testing.T) {
	unitDir := filepath.Join(t.TempDir(), "systemd", "user")
	c, out := serviceTestContext(t, map[string]string{"USER": "tester", "PATH": "/home/tester/.local/bin:/usr/bin"})
	runner := &stubRunner{linger: "Linger=no"}
	if code := serviceInstall(context.Background(), c, unitDir, "/opt/forge/forge", runner.run); code != 0 {
		t.Fatalf("install = %d\n%s", code, out.String())
	}

	wantForge := `[Unit]
Description=Forge daemon

[Service]
Type=simple
ExecStart=/opt/forge/forge daemon start --foreground
KillMode=process
Restart=on-failure
Environment=FORGE_HOME=/home/tester/.forge
Environment=PATH=/home/tester/.local/bin:/usr/bin

[Install]
WantedBy=default.target
`
	wantWorker := `[Unit]
Description=Forge worker
After=forge.service

[Service]
Type=simple
ExecStart=/opt/forge/forge worker start
KillMode=process
Restart=on-failure
Environment=FORGE_HOME=/home/tester/.forge
Environment=PATH=/home/tester/.local/bin:/usr/bin

[Install]
WantedBy=default.target
`
	for name, want := range map[string]string{"forge.service": wantForge, "forge-worker.service": wantWorker} {
		b, err := os.ReadFile(filepath.Join(unitDir, name))
		if err != nil {
			t.Fatal(err)
		}
		if string(b) != want {
			t.Errorf("%s:\n got %q\nwant %q", name, b, want)
		}
		fi, err := os.Stat(filepath.Join(unitDir, name))
		if err != nil {
			t.Fatal(err)
		}
		if fi.Mode().Perm() != 0o644 {
			t.Errorf("%s mode = %04o, want 0644", name, fi.Mode().Perm())
		}
	}

	wantCalls := []string{
		"systemctl --user daemon-reload",
		"systemctl --user enable --now forge forge-worker",
		"loginctl show-user tester --property=Linger",
	}
	if got := strings.Join(runner.calls, "\n"); got != strings.Join(wantCalls, "\n") {
		t.Errorf("calls:\n%s\nwant:\n%s", got, strings.Join(wantCalls, "\n"))
	}
	// Linger off: the install mentions enable-linger but never runs it.
	if !strings.Contains(out.String(), "loginctl enable-linger tester") {
		t.Errorf("no linger note in:\n%s", out.String())
	}
	for _, call := range runner.calls {
		if strings.Contains(call, "enable-linger") {
			t.Fatalf("linger must never be enabled by Forge: %s", call)
		}
	}
}

func TestServiceInstallLingerOnStaysQuiet(t *testing.T) {
	unitDir := t.TempDir()
	c, out := serviceTestContext(t, map[string]string{"USER": "tester"})
	runner := &stubRunner{linger: "Linger=yes"}
	if code := serviceInstall(context.Background(), c, unitDir, "/opt/forge/forge", runner.run); code != 0 {
		t.Fatalf("install = %d", code)
	}
	if strings.Contains(out.String(), "enable-linger") {
		t.Errorf("linger note printed although linger is on:\n%s", out.String())
	}
}

func TestServiceUninstallRemovesUnits(t *testing.T) {
	unitDir := t.TempDir()
	for _, name := range []string{"forge.service", "forge-worker.service"} {
		if err := os.WriteFile(filepath.Join(unitDir, name), []byte("[Unit]\n"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	c, out := serviceTestContext(t, nil)
	runner := &stubRunner{}
	if code := serviceUninstall(context.Background(), c, unitDir, runner.run); code != 0 {
		t.Fatalf("uninstall = %d\n%s", code, out.String())
	}
	for _, name := range []string{"forge.service", "forge-worker.service"} {
		if _, err := os.Stat(filepath.Join(unitDir, name)); !os.IsNotExist(err) {
			t.Errorf("%s still exists (err %v)", name, err)
		}
	}
	wantCalls := []string{
		"systemctl --user disable --now forge forge-worker",
		"systemctl --user daemon-reload",
	}
	if got := strings.Join(runner.calls, "\n"); got != strings.Join(wantCalls, "\n") {
		t.Errorf("calls:\n%s\nwant:\n%s", got, strings.Join(wantCalls, "\n"))
	}
}

func TestServiceUsage(t *testing.T) {
	c, _ := serviceTestContext(t, nil)
	var errOut strings.Builder
	c.stderr = &errOut
	if code := runService(context.Background(), c, []string{"bogus"}); code != 2 {
		t.Errorf("bogus subcommand = %d, want 2", code)
	}
	if !strings.Contains(errOut.String(), "install|uninstall|status") {
		t.Errorf("usage line missing: %q", errOut.String())
	}
}
