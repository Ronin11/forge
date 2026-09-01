package main

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"strconv"
	"strings"
	"syscall"
	"time"

	"forge/internal/controlplane"
	"forge/internal/core/protocol"
	"forge/internal/logging"
)

// listeners builds the socket and TCP listeners, adopting descriptors
// inherited across a drain restart (FORGE_SOCK_FD/FORGE_HTTP_FD, DESIGN.md
// §1.4) instead of binding fresh ones — the socket file is kept, never
// unlinked or re-created, so clients see no gap.
func (d *daemonProcess) listeners(ctx context.Context) (unixL, tcpL net.Listener, err error) {
	adopt := func(key, name string) (net.Listener, bool, error) {
		raw := d.c.getenv(key)
		if raw == "" {
			return nil, false, nil
		}
		fd, perr := strconv.Atoi(raw)
		if perr != nil || fd < 0 {
			return nil, true, fmt.Errorf("%s=%q: want a descriptor number", key, raw)
		}
		l, lerr := controlplane.ListenerFromFD(uintptr(fd), name)
		return l, true, lerr
	}
	unixL, ok, err := adopt(controlplane.EnvSockFD, controlplane.SocketFile)
	if err != nil {
		return nil, nil, err
	}
	if !ok {
		if unixL, err = controlplane.ListenSocket(d.c.forgeHome); err != nil {
			return nil, nil, err
		}
	}
	tcpL, ok, err = adopt(controlplane.EnvHTTPFD, "http")
	if err != nil {
		return nil, nil, errors.Join(err, unixL.Close())
	}
	if !ok {
		if tcpL, err = controlplane.ListenTCP(ctx, d.cfg.HTTP.Listen); err != nil {
			return nil, nil, errors.Join(err, unixL.Close())
		}
	}
	return unixL, tcpL, nil
}

// execRestart replaces this process with execPath, inheriting the lock and
// both listener descriptors so no request, connection, or lock is dropped
// (DESIGN.md §1.4). The lock fd is CLOEXEC for its whole life (children must
// never inherit it — M6 smoke 6) except here, cleared just before the exec.
// On success it never returns.
func (d *daemonProcess) execRestart(execPath string, unixL, tcpL net.Listener) error {
	// Reap plugin children first: syscall.Exec replaces this image in place, so
	// a plugin left running would orphan (and keep an inherited listener fd,
	// wedging the next bind). Kill and wait for them before the swap.
	if d.stopPlugins != nil {
		d.stopPlugins()
	}
	ul, ok := unixL.(*net.UnixListener)
	if !ok {
		return fmt.Errorf("exec restart: socket listener is %T, not *net.UnixListener", unixL)
	}
	tl, ok := tcpL.(*net.TCPListener)
	if !ok {
		return fmt.Errorf("exec restart: tcp listener is %T, not *net.TCPListener", tcpL)
	}
	uf, err := ul.File()
	if err != nil {
		return fmt.Errorf("dup socket listener: %w", err)
	}
	tf, err := tl.File()
	if err != nil {
		return errors.Join(fmt.Errorf("dup tcp listener: %w", err), uf.Close())
	}
	// The listener dups are born close-on-exec and the lock is deliberately
	// kept close-on-exec (children must never inherit it); all three must
	// survive this one exec.
	for _, f := range []*os.File{uf, tf, d.lock.File()} {
		if err := setCloexec(f.Fd(), false); err != nil {
			return errors.Join(fmt.Errorf("clear cloexec on fd %d: %w", f.Fd(), err), uf.Close(), tf.Close())
		}
	}
	argv, env := restartExecSpec(execPath, d.c.forgeHome, os.Environ(), logging.Environ(d.handler), d.lock.File().Fd(), uf.Fd(), tf.Fd())
	if err := syscall.Exec(execPath, argv, env); err != nil {
		return errors.Join(fmt.Errorf("exec %s: %w", execPath, err), uf.Close(), tf.Close())
	}
	return nil
}

// restartExecSpec assembles the argv and environment for §1.4's exec: the same
// foreground invocation the auto-start spawn uses (the lock travels by
// --lock-fd), plus the listener descriptors and the restarted marker in the
// environment. Stale copies of every key it sets are dropped from the base
// environment first — os.Getenv returns the first match, so appending alone
// would leave a second restart reading the first one's numbers.
func restartExecSpec(execPath, home string, base, logEnv []string, lockFD, sockFD, httpFD uintptr) (argv, env []string) {
	argv = []string{execPath, "daemon", "start", "--foreground", "--lock-fd", strconv.Itoa(int(lockFD))}
	drop := map[string]bool{"FORGE_HOME": true, controlplane.EnvSockFD: true, controlplane.EnvHTTPFD: true, controlplane.EnvRestarted: true}
	for _, kv := range logEnv {
		if k, _, ok := strings.Cut(kv, "="); ok {
			drop[k] = true
		}
	}
	env = make([]string, 0, len(base)+len(logEnv)+4)
	for _, kv := range base {
		if k, _, ok := strings.Cut(kv, "="); ok && drop[k] {
			continue
		}
		env = append(env, kv)
	}
	env = append(env, logEnv...)
	env = append(env,
		"FORGE_HOME="+home,
		controlplane.EnvSockFD+"="+strconv.Itoa(int(sockFD)),
		controlplane.EnvHTTPFD+"="+strconv.Itoa(int(httpFD)),
		controlplane.EnvRestarted+"=1")
	return argv, env
}

// runDaemonRestart is DESIGN.md §1.4: ask the daemon to drain and then exec
// this CLI's own binary, and wait until the handshake answers with the new
// version. The pid never changes — exec replaces the image in place.
func runDaemonRestart(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("daemon restart")
	timeout := fs.Int("timeout", 30, "seconds the daemon waits for in-flight requests before exec")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.resolveLogging(lf, "cli.daemon")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	cl.tolerateMismatch = true
	if err := cl.connect(ctx); err != nil {
		return c.fail("daemon restart", err)
	}
	self, err := os.Executable()
	if err != nil {
		return c.fail("daemon restart", fmt.Errorf("resolve forge binary: %w", err))
	}
	if err := cl.do(ctx, http.MethodPost, "/api/v1/daemon/drain", map[string]any{"exec": self, "timeout_seconds": *timeout}, nil); err != nil {
		return c.fail("daemon restart", err)
	}
	// The listener descriptors survive the exec, so polling just works; the
	// new image is recognised by its version with the state back to running
	// (the old one answers draining from the moment the drain landed).
	wait := time.Duration(*timeout)*time.Second + 30*time.Second
	deadline := time.Now().Add(wait)
	for {
		var h protocol.Handshake
		if err := cl.do(ctx, http.MethodGet, "/api/v1/handshake", nil, &h); err == nil && h.Version == version && h.State == "running" {
			fmt.Fprintf(c.stdout, "daemon restarted (pid %d)\n", h.PID)
			return 0
		}
		if time.Now().After(deadline) {
			return c.fail("daemon restart", fmt.Errorf("daemon did not come back as %s within %s; check 'forge daemon status' and the daemon log", version, wait))
		}
		select {
		case <-ctx.Done():
			return c.fail("daemon restart", ctx.Err())
		case <-time.After(200 * time.Millisecond):
		}
	}
}

// setCloexec flips FD_CLOEXEC on one descriptor. The lock fd keeps it set for
// its whole life except across the exec restart; listener dups clear it there
// too.
func setCloexec(fd uintptr, on bool) error {
	flags, _, e := syscall.Syscall(syscall.SYS_FCNTL, fd, syscall.F_GETFD, 0)
	if e != 0 {
		return fmt.Errorf("F_GETFD: %v", e)
	}
	if on {
		flags |= syscall.FD_CLOEXEC
	} else {
		flags &^= syscall.FD_CLOEXEC
	}
	if _, _, e := syscall.Syscall(syscall.SYS_FCNTL, fd, syscall.F_SETFD, flags); e != 0 {
		return fmt.Errorf("F_SETFD: %v", e)
	}
	return nil
}
