package tui

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"forge/internal/core/daemon"
	"forge/internal/core/doctor"
)

// RunDoctor is `forge doctor`: the local checks always, plus GET
// /api/v1/doctor merged in when the socket answers. It never auto-starts the
// daemon (DESIGN.md §1.2's exclusion list), so the probe is a plain dial on
// the socket, not the shared auto-starting client.
func RunDoctor(ctx context.Context, c *Context, args []string) int {
	fs, lf := c.Flags("doctor")
	asJSON := fs.Bool("json", false, "JSON output")
	if code := c.Parse(fs, args); code >= 0 {
		return code
	}
	if fs.NArg() > 0 {
		fmt.Fprintf(c.Stderr, "forge doctor: unexpected argument %q\n", fs.Arg(0))
		return 2
	}
	_, log, code := c.ResolveLogging(lf, "cli.doctor")
	if code >= 0 {
		return code
	}
	checks := localChecks(c)
	remote, err := daemonChecks(ctx, c.ForgeHome)
	switch {
	case err != nil:
		log.DebugContext(ctx, "daemon not reachable", "error", err)
		checks = append(checks, doctor.Check{Name: "api", Status: doctor.StatusWarn, Detail: "daemon not reachable; local checks only", Hint: "forge daemon start"})
	default:
		checks = append(checks, doctor.Check{Name: "api", Status: doctor.StatusOK, Detail: "daemon answered /api/v1/doctor"})
		checks = append(checks, remote...)
	}
	if *asJSON {
		c.PrintJSON(checks)
	} else {
		printChecks(c, checks)
	}
	if doctor.AnyFailed(checks) {
		return 1
	}
	return 0
}

// localChecks runs everything that works with the daemon down. Probes with a
// single home elsewhere (flock, daemon.json) are gathered here and handed to
// the doctor package's decision functions.
func localChecks(c *Context) []doctor.Check {
	home := c.ForgeHome
	checks := doctor.Binaries(exec.LookPath, binaryVersion)
	checks = append(checks,
		doctor.Home(home),
		doctor.Token(filepath.Join(home, daemon.TokenFile)),
		doctor.DB(filepath.Join(home, daemon.DBFile)),
	)
	if _, err := os.Stat(home); err == nil {
		facts := doctor.DaemonFacts{}
		if _, err := os.Stat(filepath.Join(home, daemon.SocketFile)); err == nil {
			facts.SocketExists = true
		}
		if held, err := daemon.IsLocked(home); err == nil {
			facts.LockHeld = held
		}
		if st, err := daemon.ReadState(home); err == nil && st != nil {
			facts.StateExists, facts.PID, facts.PIDAlive = true, st.PID, st.Alive()
		}
		checks = append(checks, doctor.Socket(facts), doctor.Lock(facts), doctor.Disk(home))
	}
	return checks
}

// binaryVersion reads a one-line version where that is cheap; claude and npx
// spawn a runtime to answer, so they report only their path.
func binaryVersion(name string) string {
	switch name {
	case "git", "gh", "node":
	default:
		return ""
	}
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	out, err := exec.CommandContext(ctx, name, "--version").Output()
	if err != nil {
		return ""
	}
	line, _, _ := strings.Cut(strings.TrimSpace(string(out)), "\n")
	return line
}

// daemonChecks asks the daemon for its side of the table over a plain socket
// dial — no auto-start, no lock taking.
func daemonChecks(ctx context.Context, home string) ([]doctor.Check, error) {
	sock := filepath.Join(home, daemon.SocketFile)
	if _, err := os.Stat(sock); err != nil {
		return nil, fmt.Errorf("no socket: %w", err)
	}
	client := &http.Client{
		Timeout: 5 * time.Second,
		Transport: &http.Transport{DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			var d net.Dialer
			return d.DialContext(ctx, "unix", sock)
		}},
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, "http://forge/api/v1/doctor", nil)
	if err != nil {
		return nil, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := resp.Body.Close(); cerr != nil && err == nil {
			err = cerr
		}
	}()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("GET /api/v1/doctor: status %d", resp.StatusCode)
	}
	var checks []doctor.Check
	if derr := json.NewDecoder(resp.Body).Decode(&checks); derr != nil {
		return nil, fmt.Errorf("decode doctor response: %w", derr)
	}
	return checks, nil
}

// printChecks renders the aligned table: STATUS NAME DETAIL, with the fix hint
// on its own line under any row that is not ok.
func printChecks(c *Context, checks []doctor.Check) {
	nameWidth := 0
	for _, ch := range checks {
		nameWidth = max(nameWidth, len(ch.Name))
	}
	for _, ch := range checks {
		fmt.Fprintf(c.Stdout, "%-4s  %-*s  %s\n", strings.ToUpper(ch.Status), nameWidth, ch.Name, ch.Detail)
		if ch.Status != doctor.StatusOK && ch.Hint != "" {
			fmt.Fprintf(c.Stdout, "      %-*s  hint: %s\n", nameWidth, "", ch.Hint)
		}
	}
}
