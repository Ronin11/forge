package daemon

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// Paths under the Forge home the daemon and CLI agree on.
const (
	LockFile   = "daemon.lock"
	StateFile  = "daemon.json"
	SocketFile = "forge.sock"
	TokenFile  = "token"
	DBFile     = "forge.sqlite3"
)

// Environment keys of the drain restart (DESIGN.md §1.4): the exec'd image
// adopts the listener descriptors these name instead of binding fresh ones,
// and journals daemon.restarted instead of daemon.started. The lock keeps
// travelling by the --lock-fd flag, the same convention the auto-start spawn
// uses.
const (
	EnvSockFD    = "FORGE_SOCK_FD"
	EnvHTTPFD    = "FORGE_HTTP_FD"
	EnvRestarted = "FORGE_RESTARTED"
)

// ListenerFromFD adopts a listening descriptor inherited across §1.4's exec.
// The inherited fd is closed after adoption (FileListener dups it), so
// repeated restarts never accumulate descriptors. For a unix listener the
// socket file is left exactly as it is — unlinking it would cut off clients.
func ListenerFromFD(fd uintptr, name string) (net.Listener, error) {
	f := os.NewFile(fd, name)
	l, err := net.FileListener(f)
	if err != nil {
		return nil, errors.Join(fmt.Errorf("adopt listener %s (fd %d): %w", name, fd, err), f.Close())
	}
	if err := f.Close(); err != nil {
		return nil, errors.Join(fmt.Errorf("close inherited fd %d: %w", fd, err), l.Close())
	}
	return l, nil
}

// DaemonState is <home>/daemon.json. Liveness is never inferred from it alone;
// the lock and pid identity decide (DESIGN.md §1.1).
type DaemonState struct {
	PID           int       `json:"pid"`
	PIDStart      int64     `json:"pid_start"`
	WorkerPID     int       `json:"worker_pid,omitempty"`
	Version       string    `json:"version"`
	SchemaVersion string    `json:"schema_version"`
	Socket        string    `json:"socket"`
	HTTP          string    `json:"http"`
	StartedAt     time.Time `json:"started_at"`
	State         string    `json:"state"` // running | draining
}

// Lock is the daemon lock file, held for the daemon's lifetime and handed from
// the CLI to the daemon it spawns as an inherited descriptor.
type Lock struct {
	f *os.File
}

// TryLock takes <home>/daemon.lock non-blocking; ErrLocked when someone holds it.
func TryLock(home string) (*Lock, error) {
	path := filepath.Join(home, LockFile)
	f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, fmt.Errorf("open %s: %w", path, err)
	}
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		cerr := f.Close()
		if errors.Is(err, syscall.EWOULDBLOCK) {
			return nil, errors.Join(ErrLocked, cerr)
		}
		return nil, errors.Join(fmt.Errorf("flock %s: %w", path, err), cerr)
	}
	return &Lock{f: f}, nil
}

// ErrLocked means another process holds the daemon lock.
var ErrLocked = errors.New("daemon lock is held")

// LockFromFD adopts a lock descriptor inherited from the CLI (auto-start step 4):
// flock belongs to the open file description, so the lock came with the fd.
func LockFromFD(fd uintptr) *Lock {
	return &Lock{f: os.NewFile(fd, LockFile)}
}

// File exposes the descriptor for passing to a child or across exec. The
// caller clears FD_CLOEXEC on it when it must survive exec.
func (l *Lock) File() *os.File { return l.f }

// Release closes the descriptor, which drops the lock.
func (l *Lock) Release() error {
	if l == nil || l.f == nil {
		return nil
	}
	err := l.f.Close()
	l.f = nil
	return err
}

// IsLocked reports whether the lock is currently held by anyone, without
// taking it for longer than a probe.
func IsLocked(home string) (bool, error) {
	l, err := TryLock(home)
	if errors.Is(err, ErrLocked) {
		return true, nil
	}
	if err != nil {
		return false, err
	}
	return false, l.Release()
}

// WriteState writes daemon.json atomically (tmp + rename).
func WriteState(home string, st DaemonState) error {
	path := filepath.Join(home, StateFile)
	b, err := json.MarshalIndent(st, "", "  ")
	if err != nil {
		return fmt.Errorf("encode daemon state: %w", err)
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, b, 0o600); err != nil {
		return fmt.Errorf("write %s: %w", tmp, err)
	}
	if err := os.Rename(tmp, path); err != nil {
		return fmt.Errorf("rename %s: %w", tmp, err)
	}
	return nil
}

// ReadState reads daemon.json; a missing file is (nil, nil).
func ReadState(home string) (*DaemonState, error) {
	b, err := os.ReadFile(filepath.Join(home, StateFile))
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read daemon state: %w", err)
	}
	var st DaemonState
	if err := json.Unmarshal(b, &st); err != nil {
		return nil, fmt.Errorf("decode daemon state: %w", err)
	}
	return &st, nil
}

// ProcStart returns /proc/<pid>/stat field 22, the identity that makes a pid in
// daemon.json trustworthy; 0 and an error when the process is gone.
func ProcStart(pid int) (int64, error) {
	b, err := os.ReadFile(fmt.Sprintf("/proc/%d/stat", pid))
	if err != nil {
		return 0, fmt.Errorf("read process %d: %w", pid, err)
	}
	// The comm field is parenthesised and may contain spaces; fields follow ")".
	s := string(b)
	i := strings.LastIndex(s, ")")
	if i < 0 {
		return 0, fmt.Errorf("process %d: malformed stat", pid)
	}
	fields := strings.Fields(s[i+1:])
	if len(fields) < 20 {
		return 0, fmt.Errorf("process %d: short stat", pid)
	}
	start, err := strconv.ParseInt(fields[19], 10, 64) // field 22 overall; 20th after ")"
	if err != nil {
		return 0, fmt.Errorf("process %d: start time: %w", pid, err)
	}
	return start, nil
}

// Alive reports whether daemon.json's pid is the process that wrote it.
func (st *DaemonState) Alive() bool {
	if st == nil || st.PID == 0 {
		return false
	}
	start, err := ProcStart(st.PID)
	return err == nil && start == st.PIDStart
}

// ListenSocket creates <home>/forge.sock (0600, directory expected 0700),
// unlinking a stale file first — safe because the caller holds the lock.
func ListenSocket(home string) (net.Listener, error) {
	path := filepath.Join(home, SocketFile)
	if err := os.Remove(path); err != nil && !os.IsNotExist(err) {
		return nil, fmt.Errorf("remove stale socket %s: %w", path, err)
	}
	old := syscall.Umask(0o077)
	l, err := net.Listen("unix", path)
	syscall.Umask(old)
	if err != nil {
		return nil, fmt.Errorf("listen on %s: %w", path, err)
	}
	if err := os.Chmod(path, 0o600); err != nil {
		return nil, errors.Join(fmt.Errorf("chmod socket: %w", err), l.Close())
	}
	return l, nil
}

// ListenTCP binds the loopback listener; a failure is fatal with the port named.
func ListenTCP(ctx context.Context, addr string) (net.Listener, error) {
	var lc net.ListenConfig
	l, err := lc.Listen(ctx, "tcp", addr)
	if err != nil {
		return nil, fmt.Errorf("listen on %s: %w (is another Forge, or something else, using it?)", addr, err)
	}
	return l, nil
}

// WaitForSocket polls until a connection to the socket succeeds or the deadline
// passes; the CLI's auto-start uses it (steps 2–4).
func WaitForSocket(ctx context.Context, home string, timeout time.Duration) error {
	path := filepath.Join(home, SocketFile)
	deadline := time.Now().Add(timeout)
	for {
		var d net.Dialer
		conn, err := d.DialContext(ctx, "unix", path)
		if err == nil {
			return conn.Close()
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("socket %s did not answer within %s: %w", path, timeout, err)
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(100 * time.Millisecond):
		}
	}
}
