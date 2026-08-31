package main

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// serviceRunner runs one systemctl/loginctl invocation and returns its
// combined output; tests stub it so no real unit is ever touched.
type serviceRunner func(ctx context.Context, name string, args ...string) (string, error)

// runService is `forge service install|uninstall|status`: the systemd user
// units of DESIGN.md §1.3. It never touches linger and never auto-starts the
// daemon; systemd owns both processes once installed.
func runService(ctx context.Context, c *cmdContext, args []string) int {
	sub := ""
	if len(args) > 0 && !strings.HasPrefix(args[0], "-") {
		sub, args = args[0], args[1:]
	}
	fs, lf := c.flags("service " + sub)
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	_, _, code := c.resolveLogging(lf, "cli.service")
	if code >= 0 {
		return code
	}
	switch sub {
	case "install", "uninstall", "status":
	default:
		fmt.Fprintln(c.stderr, "usage: forge service install|uninstall|status")
		return 2
	}
	if _, err := exec.LookPath("systemctl"); err != nil {
		fmt.Fprintln(c.stderr, "forge service: systemctl is not on PATH — this system does not run systemd user services")
		return 1
	}
	switch sub {
	case "install", "uninstall":
		self, err := os.Executable()
		if err != nil {
			return c.fail("service "+sub, fmt.Errorf("resolve forge binary: %w", err))
		}
		if sub == "install" {
			return serviceInstall(ctx, c, serviceUnitDir(c), self, execServiceRunner)
		}
		return serviceUninstall(ctx, c, serviceUnitDir(c), execServiceRunner)
	default:
		return serviceStatus(ctx, c)
	}
}

// serviceUnitDir is where the user units live: $XDG_CONFIG_HOME/systemd/user,
// defaulting to ~/.config/systemd/user. Tests point XDG_CONFIG_HOME at a temp
// directory so the real units are never written.
func serviceUnitDir(c *cmdContext) string {
	if x := c.getenv("XDG_CONFIG_HOME"); x != "" {
		return filepath.Join(x, "systemd", "user")
	}
	return filepath.Join(c.userHome, ".config", "systemd", "user")
}

// execServiceRunner is the real runner behind install/uninstall.
func execServiceRunner(ctx context.Context, name string, args ...string) (string, error) {
	out, err := exec.CommandContext(ctx, name, args...).CombinedOutput()
	return strings.TrimSpace(string(out)), err
}

// forgeUnit is forge.service: the daemon in the foreground under systemd.
// KillMode=process because the detached worker is not this unit's to kill.
func forgeUnit(binary, home string) string {
	return fmt.Sprintf(`[Unit]
Description=Forge daemon

[Service]
Type=simple
ExecStart=%s daemon start --foreground
KillMode=process
Restart=on-failure
Environment=FORGE_HOME=%s

[Install]
WantedBy=default.target
`, binary, home)
}

// workerUnit is forge-worker.service: same shape, ordered after the daemon.
func workerUnit(binary, home string) string {
	return fmt.Sprintf(`[Unit]
Description=Forge worker
After=forge.service

[Service]
Type=simple
ExecStart=%s worker start
KillMode=process
Restart=on-failure
Environment=FORGE_HOME=%s

[Install]
WantedBy=default.target
`, binary, home)
}

// serviceInstall writes both units, reloads systemd, and enables them now. It
// is shared with `forge init --service`. Linger is only mentioned, never
// enabled: that is the operator's call (DESIGN.md §1.3).
func serviceInstall(ctx context.Context, c *cmdContext, unitDir, binary string, run serviceRunner) int {
	if err := os.MkdirAll(unitDir, 0o755); err != nil {
		return c.fail("service install", fmt.Errorf("create %s: %w", unitDir, err))
	}
	units := map[string]string{
		"forge.service":        forgeUnit(binary, c.forgeHome),
		"forge-worker.service": workerUnit(binary, c.forgeHome),
	}
	for _, name := range []string{"forge.service", "forge-worker.service"} {
		path := filepath.Join(unitDir, name)
		if err := os.WriteFile(path, []byte(units[name]), 0o644); err != nil {
			return c.fail("service install", fmt.Errorf("write %s: %w", path, err))
		}
		fmt.Fprintf(c.stdout, "wrote %s\n", path)
	}
	for _, args := range [][]string{
		{"--user", "daemon-reload"},
		{"--user", "enable", "--now", "forge", "forge-worker"},
	} {
		out, err := run(ctx, "systemctl", args...)
		if err != nil {
			return c.fail("service install", fmt.Errorf("systemctl %s: %w: %s", strings.Join(args, " "), err, out))
		}
		fmt.Fprintf(c.stdout, "ran systemctl %s\n", strings.Join(args, " "))
	}
	if user := c.getenv("USER"); user != "" {
		if out, err := run(ctx, "loginctl", "show-user", user, "--property=Linger"); err == nil && strings.Contains(out, "Linger=no") {
			fmt.Fprintf(c.stdout, "note: linger is off — 'loginctl enable-linger %s' keeps Forge running while you are logged out (Forge never runs this for you)\n", user)
		}
	}
	return 0
}

// serviceUninstall disables both units, removes the files, and reloads.
func serviceUninstall(ctx context.Context, c *cmdContext, unitDir string, run serviceRunner) int {
	if out, err := run(ctx, "systemctl", "--user", "disable", "--now", "forge", "forge-worker"); err != nil {
		// Not-enabled units still get removed below; say what systemd said.
		fmt.Fprintf(c.stderr, "systemctl --user disable --now forge forge-worker: %v: %s\n", err, out)
	} else {
		fmt.Fprintln(c.stdout, "ran systemctl --user disable --now forge forge-worker")
	}
	for _, name := range []string{"forge.service", "forge-worker.service"} {
		path := filepath.Join(unitDir, name)
		switch err := os.Remove(path); {
		case err == nil:
			fmt.Fprintf(c.stdout, "removed %s\n", path)
		case errors.Is(err, os.ErrNotExist):
			fmt.Fprintf(c.stdout, "%s was not installed\n", path)
		default:
			return c.fail("service uninstall", fmt.Errorf("remove %s: %w", path, err))
		}
	}
	if out, err := run(ctx, "systemctl", "--user", "daemon-reload"); err != nil {
		return c.fail("service uninstall", fmt.Errorf("systemctl --user daemon-reload: %w: %s", err, out))
	}
	fmt.Fprintln(c.stdout, "ran systemctl --user daemon-reload")
	return 0
}

// serviceStatus is a passthrough: systemctl renders, its exit code is ours.
func serviceStatus(ctx context.Context, c *cmdContext) int {
	cmd := exec.CommandContext(ctx, "systemctl", "--user", "status", "forge", "forge-worker", "--no-pager")
	cmd.Stdout, cmd.Stderr = c.stdout, c.stderr
	if err := cmd.Run(); err != nil {
		var ee *exec.ExitError
		if errors.As(err, &ee) {
			return ee.ExitCode()
		}
		return c.fail("service status", err)
	}
	return 0
}
