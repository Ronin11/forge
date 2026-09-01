package tui

import (
	"bufio"
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
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"forge/internal/core/daemon"
	"forge/internal/core/logging"
	"forge/internal/core/protocol"
	"forge/internal/core/worker"
)

// cliClient is the thin client every operator command uses: it connects over
// the socket, auto-starts the daemon when needed (DESIGN.md §1.2), checks the
// handshake, and speaks JSON.
type cliClient struct {
	c       *Context
	log     *slog.Logger
	http    *http.Client
	baseURL string
	// NoAutoStart commands (daemon status, cl.c.Version…) never spawn a daemon.
	NoAutoStart bool
	// TolerateMismatch commands (daemon restart|stop|status|logs, doctor) warn
	// instead of exiting on a cl.c.Version mismatch: they are the cure.
	TolerateMismatch bool
}

func (c *Context) Client(log *slog.Logger) *cliClient {
	sock := filepath.Join(c.ForgeHome, daemon.SocketFile)
	transport := &http.Transport{DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
		var d net.Dialer
		return d.DialContext(ctx, "unix", sock)
	}}
	return &cliClient{c: c, log: log, http: &http.Client{Transport: transport, Timeout: 30 * time.Second}, baseURL: "http://forge"}
}

// errMismatch is returned when the daemon and CLI versions differ.
var errMismatch = errors.New("cl.c.Version mismatch")

// connect implements the auto-start steps and returns once a handshake
// succeeded (or the mismatch line was printed).
func (cl *cliClient) Connect(ctx context.Context) error {
	home := cl.c.ForgeHome
	// The home must exist before the lock file can (a fresh box); bootstrap
	// proper runs in the daemon.
	if err := os.MkdirAll(home, 0o700); err != nil {
		return fmt.Errorf("create %s: %w", home, err)
	}
	if err := cl.tryHandshake(ctx, 2*time.Second); err == nil {
		return nil
	} else if errors.Is(err, errMismatch) {
		return err
	}
	if cl.NoAutoStart {
		return fmt.Errorf("daemon is not running (start it with 'forge daemon start')")
	}
	// Step 2: systemd owns the default home.
	unit := filepath.Join(cl.c.UserHome, ".config", "systemd", "user", "forge.service")
	if _, err := os.Stat(unit); err == nil && (cl.c.Getenv("FORGE_HOME") == "" || home == filepath.Join(cl.c.UserHome, ".forge")) {
		out, err := exec.CommandContext(ctx, "systemctl", "--user", "start", "forge").CombinedOutput()
		if err != nil {
			return fmt.Errorf("systemctl --user start forge: %w: %s", err, strings.TrimSpace(string(out)))
		}
		if err := daemon.WaitForSocket(ctx, home, 10*time.Second); err != nil {
			return err
		}
		return cl.tryHandshake(ctx, 2*time.Second)
	}
	// Step 3: the lock.
	lock, err := daemon.TryLock(home)
	if errors.Is(err, daemon.ErrLocked) {
		cl.log.DebugContext(ctx, "daemon lock held; waiting for the socket")
		if werr := daemon.WaitForSocket(ctx, home, 10*time.Second); werr != nil {
			st, serr := daemon.ReadState(home)
			if serr != nil {
				return errors.Join(werr, serr)
			}
			if st != nil && !st.Alive() {
				return fmt.Errorf("daemon lock is held but daemon.json names dead pid %d — remove %s if no forge daemon is running", st.PID, filepath.Join(home, daemon.LockFile))
			}
			if st != nil {
				return fmt.Errorf("daemon pid %d is running but %s is missing — run 'forge daemon restart'", st.PID, filepath.Join(home, daemon.SocketFile))
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
	if err := daemon.WaitForSocket(ctx, home, 5*time.Second); err != nil {
		return fmt.Errorf("daemon started but did not answer: %w (see %s)", err, filepath.Join(home, "logs", "daemon.stdio.log"))
	}
	return cl.tryHandshake(ctx, 2*time.Second)
}

// spawnDaemon starts `forge daemon start --foreground` detached with the lock
// fd inherited (auto-start step 4) and waits for daemon.json to show its pid.
func (cl *cliClient) spawnDaemon(ctx context.Context, lock *daemon.Lock) error {
	home := cl.c.ForgeHome
	self, err := os.Executable()
	if err != nil {
		return fmt.Errorf("resolve forge binary: %w", err)
	}
	if err := os.MkdirAll(filepath.Join(home, "logs"), 0o700); err != nil {
		return fmt.Errorf("create logs dir: %w", err)
	}
	if err := os.Remove(filepath.Join(home, daemon.SocketFile)); err != nil && !os.IsNotExist(err) {
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
		st, err := daemon.ReadState(home)
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
		err := cl.Do(ctx, http.MethodGet, "/api/v1/handshake", nil, &h)
		if err == nil {
			if h.Version != cl.c.Version {
				line := fmt.Sprintf("forge: daemon is %s, this CLI is %s — run 'forge daemon restart'", h.Version, cl.c.Version)
				if cl.TolerateMismatch {
					fmt.Fprintln(cl.c.Stderr, line+" (continuing)")
					return nil
				}
				fmt.Fprintln(cl.c.Stderr, line)
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
func (cl *cliClient) Do(ctx context.Context, method, path string, in, out any) (err error) {
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

// stream opens an SSE GET and returns the response body. Unlike do it uses no
// client timeout: the stream lives until the Work ends or the daemon drains.
func (cl *cliClient) stream(ctx context.Context, path string) (io.ReadCloser, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, cl.baseURL+path, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Accept", "text/event-stream")
	client := &http.Client{Transport: cl.http.Transport}
	resp, err := client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("GET %s: %w", path, err)
	}
	if resp.StatusCode != http.StatusOK {
		msg, rerr := io.ReadAll(io.LimitReader(resp.Body, 4<<10))
		cerr := resp.Body.Close()
		if rerr != nil {
			return nil, errors.Join(&worker.StatusError{Status: resp.StatusCode, Message: rerr.Error()}, cerr)
		}
		var e protocol.Error
		if json.Unmarshal(msg, &e) == nil && e.Error != "" {
			return nil, errors.Join(&worker.StatusError{Status: resp.StatusCode, Message: e.Error}, cerr)
		}
		return nil, errors.Join(&worker.StatusError{Status: resp.StatusCode, Message: strings.TrimSpace(string(msg))}, cerr)
	}
	return resp.Body, nil
}

// sseEvent is one parsed server-sent event.
type sseEvent struct {
	event string
	id    string
	data  string
	retry bool
}

// readSSE reads one event (terminated by a blank line) from an SSE stream.
// io.EOF means the server closed it; a partial event at EOF is dropped, which
// is safe because clients resume from the last id they applied.
func readSSE(br *bufio.Reader) (sseEvent, error) {
	var ev sseEvent
	var got bool
	for {
		line, err := br.ReadString('\n')
		if err != nil {
			return ev, err
		}
		line = strings.TrimRight(line, "\r\n")
		if line == "" {
			if got {
				return ev, nil
			}
			continue
		}
		field, value, _ := strings.Cut(line, ":")
		value = strings.TrimPrefix(value, " ")
		switch field {
		case "event":
			ev.event, got = value, true
		case "id":
			ev.id, got = value, true
		case "data":
			if ev.data != "" {
				ev.data += "\n"
			}
			ev.data, got = ev.data+value, true
		case "retry":
			ev.retry, got = true, true
		}
	}
}

// fail prints one line and returns the exit code for the error kind.
func (c *Context) Fail(command string, err error) int {
	var se *worker.StatusError
	switch {
	case errors.Is(err, errMismatch):
		return 1
	case errors.As(err, &se):
		fmt.Fprintf(c.Stderr, "forge %s: %s\n", command, se.Message)
		if se.Status == 400 || se.Status == 404 {
			return 2
		}
		return 1
	default:
		fmt.Fprintf(c.Stderr, "forge %s: %v\n", command, err)
		return 1
	}
}

// printJSON is the --json output every list/show command supports.
func (c *Context) PrintJSON(v any) {
	enc := json.NewEncoder(c.Stdout)
	enc.SetIndent("", "  ")
	if err := enc.Encode(v); err != nil {
		fmt.Fprintln(c.Stderr, "forge: encode output:", err)
	}
}

// resolveLogging is the common tail of every command's flag parsing.
func (c *Context) ResolveLogging(lf *logging.Flags, component string) (*logging.Handler, *slog.Logger, int) {
	h, log, err := c.Logger(lf, logging.Config{}, component)
	if err != nil {
		fmt.Fprintln(c.Stderr, "forge:", err)
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

// TailFile follows path from offset, polling its size every 500 ms and copying
// what appeared; a shrink (rotation) restarts from the top of the new file.
// It returns nil when ctx ends — Ctrl-C is how the human leaves.
func TailFile(ctx context.Context, out io.Writer, path string, offset int64) error {
	ctx, stop := signal.NotifyContext(ctx, syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	for {
		select {
		case <-ctx.Done():
			return nil
		case <-time.After(500 * time.Millisecond):
		}
		fi, err := os.Stat(path)
		if err != nil {
			return err
		}
		if fi.Size() < offset {
			offset = 0
		}
		if fi.Size() == offset {
			continue
		}
		n, err := copyFrom(out, path, offset)
		offset += n
		if err != nil {
			return err
		}
	}
}

// copyFrom copies path's bytes from offset to out and reports how many.
func copyFrom(out io.Writer, path string, offset int64) (n int64, err error) {
	f, err := os.Open(path)
	if err != nil {
		return 0, err
	}
	defer func() { err = errors.Join(err, f.Close()) }()
	if _, err := f.Seek(offset, io.SeekStart); err != nil {
		return 0, fmt.Errorf("seek %s: %w", path, err)
	}
	n, err = io.Copy(out, f)
	return n, err
}
