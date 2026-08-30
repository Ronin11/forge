package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"forge/internal/controlplane"
	"forge/internal/logging"
	"forge/internal/protocol"
	"forge/internal/worker"
)

// cliClient is the thin client every operator command uses: it connects over
// the socket, auto-starts the daemon when needed (DESIGN.md §1.2), checks the
// handshake, and speaks JSON.
type cliClient struct {
	c       *cmdContext
	log     *slog.Logger
	http    *http.Client
	baseURL string
	// noAutoStart commands (daemon status, version…) never spawn a daemon.
	noAutoStart bool
	// tolerateMismatch commands (daemon restart|stop|status|logs, doctor) warn
	// instead of exiting on a version mismatch: they are the cure.
	tolerateMismatch bool
}

func (c *cmdContext) client(log *slog.Logger) *cliClient {
	sock := filepath.Join(c.forgeHome, controlplane.SocketFile)
	transport := &http.Transport{DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
		var d net.Dialer
		return d.DialContext(ctx, "unix", sock)
	}}
	return &cliClient{c: c, log: log, http: &http.Client{Transport: transport, Timeout: 30 * time.Second}, baseURL: "http://forge"}
}

// errMismatch is returned when the daemon and CLI versions differ.
var errMismatch = errors.New("version mismatch")

// connect implements the auto-start steps and returns once a handshake
// succeeded (or the mismatch line was printed).
func (cl *cliClient) connect(ctx context.Context) error {
	home := cl.c.forgeHome
	if err := cl.tryHandshake(ctx, 2*time.Second); err == nil {
		return nil
	} else if errors.Is(err, errMismatch) {
		return err
	}
	if cl.noAutoStart {
		return fmt.Errorf("daemon is not running (start it with 'forge daemon start')")
	}
	// Step 2: systemd owns the default home.
	unit := filepath.Join(cl.c.userHome, ".config", "systemd", "user", "forge.service")
	if _, err := os.Stat(unit); err == nil && (cl.c.getenv("FORGE_HOME") == "" || home == filepath.Join(cl.c.userHome, ".forge")) {
		out, err := exec.CommandContext(ctx, "systemctl", "--user", "start", "forge").CombinedOutput()
		if err != nil {
			return fmt.Errorf("systemctl --user start forge: %w: %s", err, strings.TrimSpace(string(out)))
		}
		if err := controlplane.WaitForSocket(ctx, home, 10*time.Second); err != nil {
			return err
		}
		return cl.tryHandshake(ctx, 2*time.Second)
	}
	// Step 3: the lock.
	lock, err := controlplane.TryLock(home)
	if errors.Is(err, controlplane.ErrLocked) {
		cl.log.DebugContext(ctx, "daemon lock held; waiting for the socket")
		if werr := controlplane.WaitForSocket(ctx, home, 10*time.Second); werr != nil {
			st, serr := controlplane.ReadState(home)
			if serr != nil {
				return errors.Join(werr, serr)
			}
			if st != nil && !st.Alive() {
				return fmt.Errorf("daemon lock is held but daemon.json names dead pid %d — remove %s if no forge daemon is running", st.PID, filepath.Join(home, controlplane.LockFile))
			}
			if st != nil {
				return fmt.Errorf("daemon pid %d is running but %s is missing — run 'forge daemon restart'", st.PID, filepath.Join(home, controlplane.SocketFile))
			}
			return werr
		}
		return cl.tryHandshake(ctx, 2*time.Second)
	}
	if err != nil {
		return err
	}
	// Step 4: spawn, handing over the locked descriptor.
	if err := cl.spawnDaemon(ctx, lock); err != nil {
		return errors.Join(err, lock.Release())
	}
	if err := lock.Release(); err != nil {
		return err
	}
	if err := controlplane.WaitForSocket(ctx, home, 5*time.Second); err != nil {
		return fmt.Errorf("daemon started but did not answer: %w (see %s)", err, filepath.Join(home, "logs", "daemon.stdio.log"))
	}
	return cl.tryHandshake(ctx, 2*time.Second)
}

// spawnDaemon starts `forge daemon start --foreground` detached with the lock
// fd inherited (auto-start step 4) and waits for daemon.json to show its pid.
func (cl *cliClient) spawnDaemon(ctx context.Context, lock *controlplane.Lock) error {
	home := cl.c.forgeHome
	self, err := os.Executable()
	if err != nil {
		return fmt.Errorf("resolve forge binary: %w", err)
	}
	if err := os.MkdirAll(filepath.Join(home, "logs"), 0o700); err != nil {
		return fmt.Errorf("create logs dir: %w", err)
	}
	if err := os.Remove(filepath.Join(home, controlplane.SocketFile)); err != nil && !os.IsNotExist(err) {
		return fmt.Errorf("remove stale socket: %w", err)
	}
	stdio, err := os.OpenFile(filepath.Join(home, "logs", "daemon.stdio.log"), os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
	if err != nil {
		return fmt.Errorf("open daemon stdio log: %w", err)
	}
	defer func() {
		if cerr := stdio.Close(); cerr != nil {
			cl.log.WarnContext(ctx, "close stdio log", "error", cerr)
		}
	}()
	cmd := exec.Command(self, "daemon", "start", "--foreground", "--lock-fd", "3")
	cmd.Env = worker.PassthroughEnv(os.Environ(), "FORGE_HOME="+home, "FORGE_LOG_LEVEL=debug", "FORGE_LOG_FORMAT=json")
	cmd.Stdin, cmd.Stdout, cmd.Stderr = nil, stdio, stdio
	cmd.ExtraFiles = []*os.File{lock.File()}
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("spawn daemon: %w", err)
	}
	pid := cmd.Process.Pid
	if err := cmd.Process.Release(); err != nil {
		return fmt.Errorf("release daemon process: %w", err)
	}
	cl.log.InfoContext(ctx, "daemon spawned", "pid", pid)
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		st, err := controlplane.ReadState(home)
		if err == nil && st != nil && st.PID == pid {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(50 * time.Millisecond):
		}
	}
	tail, rerr := os.ReadFile(filepath.Join(home, "logs", "daemon.stdio.log"))
	if rerr != nil {
		tail = []byte("(" + rerr.Error() + ")")
	}
	if len(tail) > 2000 {
		tail = tail[len(tail)-2000:]
	}
	return fmt.Errorf("daemon %d did not write daemon.json within 5 s; last output:\n%s", pid, tail)
}

// tryHandshake connects (retrying for up to wait) and checks versions.
func (cl *cliClient) tryHandshake(ctx context.Context, wait time.Duration) error {
	deadline := time.Now().Add(wait)
	for {
		var h protocol.Handshake
		err := cl.do(ctx, http.MethodGet, "/api/v1/handshake", nil, &h)
		if err == nil {
			if h.Version != version {
				line := fmt.Sprintf("forge: daemon is %s, this CLI is %s — run 'forge daemon restart'", h.Version, version)
				if cl.tolerateMismatch {
					fmt.Fprintln(cl.c.stderr, line+" (continuing)")
					return nil
				}
				fmt.Fprintln(cl.c.stderr, line)
				return errMismatch
			}
			return nil
		}
		var se *worker.StatusError
		if errors.As(err, &se) {
			return err
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("%w", err)
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(100 * time.Millisecond):
		}
	}
}

// do is one JSON request over the socket.
func (cl *cliClient) do(ctx context.Context, method, path string, in, out any) (err error) {
	var body io.Reader
	if in != nil {
		b, err := json.Marshal(in)
		if err != nil {
			return fmt.Errorf("encode request: %w", err)
		}
		body = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, cl.baseURL+path, body)
	if err != nil {
		return err
	}
	if in != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := cl.http.Do(req)
	if err != nil {
		return fmt.Errorf("%s %s: %w", method, path, err)
	}
	defer func() {
		if cerr := resp.Body.Close(); cerr != nil && err == nil {
			err = cerr
		}
	}()
	if resp.StatusCode == http.StatusNoContent {
		return nil
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		msg, rerr := io.ReadAll(io.LimitReader(resp.Body, 4<<10))
		if rerr != nil {
			return &worker.StatusError{Status: resp.StatusCode, Message: rerr.Error()}
		}
		var e protocol.Error
		if json.Unmarshal(msg, &e) == nil && e.Error != "" {
			return &worker.StatusError{Status: resp.StatusCode, Message: e.Error}
		}
		return &worker.StatusError{Status: resp.StatusCode, Message: strings.TrimSpace(string(msg))}
	}
	if out == nil {
		_, err = io.Copy(io.Discard, resp.Body)
		return err
	}
	return json.NewDecoder(resp.Body).Decode(out)
}

// fail prints one line and returns the exit code for the error kind.
func (c *cmdContext) fail(command string, err error) int {
	var se *worker.StatusError
	switch {
	case errors.Is(err, errMismatch):
		return 1
	case errors.As(err, &se):
		fmt.Fprintf(c.stderr, "forge %s: %s\n", command, se.Message)
		if se.Status == 400 || se.Status == 404 {
			return 2
		}
		return 1
	default:
		fmt.Fprintf(c.stderr, "forge %s: %v\n", command, err)
		return 1
	}
}

// printJSON is the --json output every list/show command supports.
func (c *cmdContext) printJSON(v any) {
	enc := json.NewEncoder(c.stdout)
	enc.SetIndent("", "  ")
	if err := enc.Encode(v); err != nil {
		fmt.Fprintln(c.stderr, "forge: encode output:", err)
	}
}

// resolveLogging is the common tail of every command's flag parsing.
func (c *cmdContext) resolveLogging(lf *logging.Flags, component string) (*logging.Handler, *slog.Logger, int) {
	h, log, err := c.logger(lf, logging.Config{}, component)
	if err != nil {
		fmt.Fprintln(c.stderr, "forge:", err)
		return nil, nil, 2
	}
	return h, log, -1
}

// short renders an id for tables.
func short(id string) string {
	if len(id) > 8 {
		return id[:8]
	}
	return id
}

// ago renders a duration since t for tables.
func ago(t time.Time, now time.Time) string {
	if t.IsZero() {
		return "-"
	}
	d := now.Sub(t).Round(time.Second)
	switch {
	case d < time.Minute:
		return fmt.Sprintf("%ds", int(d.Seconds()))
	case d < time.Hour:
		return fmt.Sprintf("%dm", int(d.Minutes()))
	case d < 48*time.Hour:
		return fmt.Sprintf("%dh", int(d.Hours()))
	}
	return fmt.Sprintf("%dd", int(d.Hours()/24))
}
