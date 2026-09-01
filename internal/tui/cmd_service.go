package tui

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

// RunService is `forge service install|uninstall|status`: the systemd user
// units of DESIGN.md §1.3. It never touches linger and never auto-starts the
// daemon; systemd owns both processes once installed.
func RunService(ctx context.Context, c *Context, args []string) int {
	sub := ""
	if len(args) > 0 && !strings.HasPrefix(args[0], "-") {
		sub, args = args[0], args[1:]
	}
	fs, lf := c.Flags("service " + sub)
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	_, _, code := c.ResolveLogging(lf, "cli.service")
	if code >= 0 {
		return code
	}
	switch sub {
	case "install", "uninstall", "status":
	default:
		fmt.Fprintln(c.Stderr, "usage: forge service install|uninstall|status")
		return 2
	}
	if _, err := exec.LookPath("systemctl"); err != nil {
		fmt.Fprintln(c.Stderr, "forge service: systemctl is not on PATH — this system does not run systemd user services")
		return 1
	}
	switch sub {
	case "install", "uninstall":
		self, err := os.Executable()
		if err != nil {
			return c.Fail("service "+sub, fmt.Errorf("resolve forge binary: %w", err))
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
func serviceUnitDir(c *Context) string {
	if x := c.Getenv("XDG_CONFIG_HOME"); x != "" {
		return filepath.Join(x, "systemd", "user")
	}
	return filepath.Join(c.UserHome, ".config", "systemd", "user")
}

// execServiceRunner is the real runner behind install/uninstall.
func execServiceRunner(ctx context.Context, name string, args ...string) (string, error) {
	out, err := exec.CommandContext(ctx, name, args...).CombinedOutput()
	return strings.TrimSpace(string(out)), err
}

// environmentLines renders the [Service] Environment= lines. PATH is captured
// from the installing shell: user units started at boot get systemd's stock
// PATH (/usr/local/bin:/usr/bin), which misses version managers like mise, so
// the worker cannot find the agent executor's binary until it is baked in.
func environmentLines(home, path string) string {
	lines := "Environment=FORGE_HOME=" + home
	if path != "" {
		lines += "\nEnvironment=PATH=" + path
	}
	return lines
}

// forgeUnit is forge.service: the daemon in the foreground under systemd.
// KillMode=process because the detached worker is not this unit's to kill.
func forgeUnit(binary, home, path string) string {
	return fmt.Sprintf(`[Unit]
Description=Forge daemon

[Service]
Type=simple
ExecStart=%s daemon start --foreground
KillMode=process
Restart=on-failure
%s

[Install]
WantedBy=default.target
`, binary, environmentLines(home, path))
}

// workerUnit is forge-worker.service: same shape, ordered after the daemon.
func workerUnit(binary, home, path string) string {
	return fmt.Sprintf(`[Unit]
Description=Forge worker
After=forge.service

[Service]
Type=simple
ExecStart=%s worker start
KillMode=process
Restart=on-failure
%s

[Install]
WantedBy=default.target
`, binary, environmentLines(home, path))
}

// serviceInstall writes both units, reloads systemd, and enables them now. It
// is shared with `forge init --service`. Linger is only mentioned, never
// enabled: that is the operator's call (DESIGN.md §1.3).
func serviceInstall(ctx context.Context, c *Context, unitDir, binary string, run serviceRunner) int {
	if err := os.MkdirAll(unitDir, 0o755); err != nil {
		return c.Fail("service install", fmt.Errorf("create %s: %w", unitDir, err))
	}
	units := map[string]string{
		"forge.service":        forgeUnit(binary, c.ForgeHome, c.Getenv("PATH")),
		"forge-worker.service": workerUnit(binary, c.ForgeHome, c.Getenv("PATH")),
	}
	for _, name := range []string{"forge.service", "forge-worker.service"} {
		path := filepath.Join(unitDir, name)
		if err := os.WriteFile(path, []byte(units[name]), 0o644); err != nil {
			return c.Fail("service install", fmt.Errorf("write %s: %w", path, err))
		}
		fmt.Fprintf(c.Stdout, "wrote %s\n", path)
	}
	for _, args := range [][]string{
		{"--user", "daemon-reload"},
		{"--user", "enable", "--now", "forge", "forge-worker"},
	} {
		out, err := run(ctx, "systemctl", args...)
		if err != nil {
			return c.Fail("service install", fmt.Errorf("systemctl %s: %w: %s", strings.Join(args, " "), err, out))
		}
		fmt.Fprintf(c.Stdout, "ran systemctl %s\n", strings.Join(args, " "))
	}
	if user := c.Getenv("USER"); user != "" {
		if out, err := run(ctx, "loginctl", "show-user", user, "--property=Linger"); err == nil && strings.Contains(out, "Linger=no") {
			fmt.Fprintf(c.Stdout, "note: linger is off — 'loginctl enable-linger %s' keeps Forge running while you are logged out (Forge never runs this for you)\n", user)
		}
	}
	return 0
}

// serviceUninstall disables both units, removes the files, and reloads.
func serviceUninstall(ctx context.Context, c *Context, unitDir string, run serviceRunner) int {
	if out, err := run(ctx, "systemctl", "--user", "disable", "--now", "forge", "forge-worker"); err != nil {
		// Not-enabled units still get removed below; say what systemd said.
		fmt.Fprintf(c.Stderr, "systemctl --user disable --now forge forge-worker: %v: %s\n", err, out)
	} else {
		fmt.Fprintln(c.Stdout, "ran systemctl --user disable --now forge forge-worker")
	}
	for _, name := range []string{"forge.service", "forge-worker.service"} {
		path := filepath.Join(unitDir, name)
		switch err := os.Remove(path); {
		case err == nil:
			fmt.Fprintf(c.Stdout, "removed %s\n", path)
		case errors.Is(err, os.ErrNotExist):
			fmt.Fprintf(c.Stdout, "%s was not installed\n", path)
		default:
			return c.Fail("service uninstall", fmt.Errorf("remove %s: %w", path, err))
		}
	}
	if out, err := run(ctx, "systemctl", "--user", "daemon-reload"); err != nil {
		return c.Fail("service uninstall", fmt.Errorf("systemctl --user daemon-reload: %w: %s", err, out))
	}
	fmt.Fprintln(c.Stdout, "ran systemctl --user daemon-reload")
	return 0
}

// serviceStatus is a passthrough: systemctl renders, its exit code is ours.
func serviceStatus(ctx context.Context, c *Context) int {
	cmd := exec.CommandContext(ctx, "systemctl", "--user", "status", "forge", "forge-worker", "--no-pager")
	cmd.Stdout, cmd.Stderr = c.Stdout, c.Stderr
	if err := cmd.Run(); err != nil {
		var ee *exec.ExitError
		if errors.As(err, &ee) {
			return ee.ExitCode()
		}
		return c.Fail("service status", err)
	}
	return 0
}
