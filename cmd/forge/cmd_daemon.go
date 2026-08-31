package main

import (
	"context"
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

	"golang.org/x/sync/errgroup"

	"forge/internal/controlplane"
	"forge/internal/kb"
	"forge/internal/logging"
	"forge/internal/modes"
	"forge/internal/modes/all"
	"forge/internal/store"
	"forge/internal/tools"
	"forge/internal/worker"
)

func runDaemon(ctx context.Context, c *cmdContext, args []string) int {
	if len(args) == 0 || strings.HasPrefix(args[0], "-") {
		fmt.Fprintln(c.stderr, "usage: forge daemon start|stop|restart|status|logs|log-level [flags]")
		return 2
	}
	switch args[0] {
	case "start":
		return runDaemonStart(ctx, c, args[1:])
	case "stop":
		return runDaemonStop(ctx, c, args[1:])
	case "restart":
		return runDaemonRestart(ctx, c, args[1:])
	case "status":
		return runDaemonStatus(ctx, c, args[1:])
	case "logs":
		return runDaemonLogs(ctx, c, args[1:])
	case "log-level":
		return runDaemonLogLevel(ctx, c, args[1:])
	}
	fmt.Fprintf(c.stderr, "forge daemon: unknown subcommand %q\n", args[0])
	return 2
}

// runDaemonStart is the daemon itself with --foreground, or a detached spawn
// through the auto-start path otherwise.
func runDaemonStart(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("daemon start")
	foreground := fs.Bool("foreground", false, "run in this process (what the detached form and systemd use)")
	lockFD := fs.Int("lock-fd", -1, "inherited daemon.lock descriptor (internal: set by the auto-start spawn)")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if !*foreground {
		_, log, code := c.resolveLogging(lf, "cli.daemon")
		if code >= 0 {
			return code
		}
		cl := c.client(log)
		cl.tolerateMismatch = true
		if err := cl.connect(ctx); err != nil {
			return c.fail("daemon start", err)
		}
		st, err := controlplane.ReadState(c.forgeHome)
		if err != nil || st == nil {
			return c.fail("daemon start", fmt.Errorf("daemon answered but daemon.json is missing"))
		}
		fmt.Fprintf(c.stdout, "daemon running (pid %d, %s)\n", st.PID, st.Version)
		return 0
	}
	home := c.forgeHome
	if err := os.MkdirAll(home, 0o700); err != nil {
		fmt.Fprintln(c.stderr, "forge daemon:", err)
		return 1
	}
	cfg, err := controlplane.LoadConfig(filepath.Join(home, "config.toml"), home, c.userHome, c.getenv)
	if err != nil {
		fmt.Fprintln(c.stderr, "forge daemon:", err)
		return 1
	}
	opts, err := logging.Resolve(lf, c.getenv, cfg.Log, home)
	if err != nil {
		fmt.Fprintln(c.stderr, "forge daemon:", err)
		return 2
	}
	sink, err := logging.OpenFileSink("daemon", opts.File)
	if err != nil {
		fmt.Fprintln(c.stderr, "forge daemon:", err)
		return 1
	}
	defer func() {
		if err := sink.Close(); err != nil {
			fmt.Fprintln(c.stderr, "forge daemon: close log:", err)
		}
	}()
	handler := logging.New(c.stderr, opts, sink)
	log := handler.For("daemon")
	d := &daemonProcess{c: c, cfg: cfg, handler: handler, log: log}
	if err := d.run(ctx, *lockFD); err != nil {
		log.ErrorContext(ctx, "daemon exited with error", "error", err)
		fmt.Fprintln(c.stderr, "forge daemon:", err)
		return 1
	}
	return 0
}

// daemonProcess is one running daemon.
type daemonProcess struct {
	c       *cmdContext
	cfg     *controlplane.Config
	handler *logging.Handler
	log     *slog.Logger
	lock    *controlplane.Lock
	state   controlplane.DaemonState
}

func (d *daemonProcess) run(ctx context.Context, lockFD int) (err error) {
	home := d.c.forgeHome
	// The lock: inherited from the CLI, or taken now.
	if lockFD >= 0 {
		d.lock = controlplane.LockFromFD(uintptr(lockFD))
	} else {
		l, lerr := controlplane.TryLock(home)
		if errors.Is(lerr, controlplane.ErrLocked) {
			return fmt.Errorf("another forge daemon owns %s", home)
		}
		if lerr != nil {
			return lerr
		}
		d.lock = l
	}
	defer func() { err = errors.Join(err, d.lock.Release()) }()
	// Keep the descriptor across exec (drain restart) — clear FD_CLOEXEC.
	if _, _, e := syscall.Syscall(syscall.SYS_FCNTL, d.lock.File().Fd(), syscall.F_SETFD, 0); e != 0 {
		return fmt.Errorf("clear cloexec on lock: %v", e)
	}
	ctx, stop := signal.NotifyContext(ctx, syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	st, err := store.Open(ctx, filepath.Join(home, controlplane.DBFile), store.Options{Logger: d.handler.For("store")})
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, st.Close()) }()
	self, err := os.Executable()
	if err != nil {
		return fmt.Errorf("resolve forge binary: %w", err)
	}
	report, err := controlplane.Bootstrap(ctx, st, controlplane.BootstrapOptions{
		Home: home, UserHome: d.c.userHome, Logger: d.handler.For("daemon.bootstrap"),
		WriteWorkerConfig: func(path string) (bool, error) { return worker.WriteDefault(path, home, self) },
		ModeSeeds:         modes.Seeds(all.All()),
	})
	if err != nil {
		return fmt.Errorf("bootstrap: %w", err)
	}
	token, err := controlplane.ReadToken(home)
	if err != nil {
		return err
	}
	registry, err := modes.NewRegistry(all.All())
	if err != nil {
		return fmt.Errorf("mode registry: %w", err)
	}
	toolRegistry := tools.Defaults()
	for _, t := range tools.LoadScriptTools(ctx, filepath.Join(home, "tools"), d.handler.For("tools.script")) {
		if err := toolRegistry.Register(t); err != nil {
			d.log.WarnContext(ctx, "script tool not registered", "tool", t.Name(), "error", err)
		}
	}
	policy := controlplane.NewBudgetPolicy(st, d.cfg.Budget, time.Now)
	unixL, tcpL, err := d.listeners(ctx)
	if err != nil {
		return err
	}
	defer func() {
		// Serve already closed both on the normal path; only the error paths
		// between here and Serve still hold them open.
		for _, l := range []net.Listener{unixL, tcpL} {
			if cerr := l.Close(); cerr != nil && !errors.Is(cerr, net.ErrClosed) {
				err = errors.Join(err, cerr)
			}
		}
	}()
	srv, err := controlplane.NewServer(controlplane.ServerOptions{
		ExecRestart: func(execPath string) error { return d.execRestart(execPath, unixL, tcpL) },
		Store:       st, Policy: policy, Logger: d.handler.For("controlplane.http"), Version: version, Token: token, Home: home, Modes: registry,
		RequiredLevel: func(string) int { return 1 },
		AllowHosts:    d.cfg.Sandbox.AllowHosts,
		KbDir:         d.cfg.KB.Path,
		Tools:         toolRegistry,
		SetLogLevels: func(spec string) error {
			levels, err := logging.ParseLevels(spec, slog.LevelInfo)
			if err != nil {
				return err
			}
			d.handler.SetLevels(levels)
			return nil
		},
		LogLevels: func() string { return d.handler.Levels().String() },
	})
	if err != nil {
		return err
	}
	ui, err := controlplane.NewUI(st, d.handler.For("controlplane.ui"), nil)
	if err != nil {
		return err
	}
	srv.MountUI(ui)
	pid := os.Getpid()
	pidStart, err := controlplane.ProcStart(pid)
	if err != nil {
		return err
	}
	d.state = controlplane.DaemonState{PID: pid, PIDStart: pidStart, Version: version, SchemaVersion: st.SchemaVersion(), Socket: filepath.Join(home, controlplane.SocketFile), HTTP: d.cfg.HTTP.Listen, StartedAt: time.Now().UTC(), State: "running"}
	if err := controlplane.WriteState(home, d.state); err != nil {
		return err
	}
	// After §1.4's exec the image is new but the process is the same; the
	// journal says so, and daemon.json (written running above) replaces the
	// draining state the old image left behind.
	journalKind := "daemon.started"
	if d.c.getenv(controlplane.EnvRestarted) == "1" {
		journalKind = "daemon.restarted"
	}
	if err := st.Write(ctx, func(tx *store.Tx) error {
		return tx.Journal(ctx, journalKind, store.EntityDaemon, "daemon", map[string]any{"pid": pid, "version": version, "created": len(report.Created)})
	}); err != nil {
		return err
	}
	d.log.InfoContext(ctx, "daemon started", "pid", pid, "version", version, "restarted", journalKind == "daemon.restarted", "socket", d.state.Socket, "http", d.cfg.HTTP.Listen, "log_file", filepath.Join(opts(d).File.Dir, "daemon.log"))

	g, gctx := errgroup.WithContext(ctx)
	g.Go(func() error { return srv.Serve(gctx, unixL, tcpL) })
	g.Go(func() error { srv.RunSweeper(gctx, 10*time.Second, d.cfg.Reflection); return nil })
	g.Go(func() error { d.kbReindexLoop(gctx, st); return nil })
	g.Go(func() error { d.nightlyPrune(gctx, st); return nil })
	g.Go(func() error { logging.WatchSIGUSR1(d.handler, d.log, nil).Run(gctx); return nil })
	g.Go(func() error { return d.ensureWorker(gctx, self) })
	err = g.Wait()
	if errors.Is(err, context.Canceled) {
		err = nil
	}
	d.log.InfoContext(context.WithoutCancel(ctx), "daemon stopped", "error", err)
	if rerr := os.Remove(d.state.Socket); rerr != nil && !os.IsNotExist(rerr) {
		err = errors.Join(err, rerr)
	}
	return err
}

func opts(d *daemonProcess) logging.Options {
	o, err := logging.Resolve(&logging.Flags{}, d.c.getenv, d.cfg.Log, d.c.forgeHome)
	if err != nil {
		// Config was validated at load; this cannot fail, but never hide it.
		d.log.Warn("resolve logging options", "error", err)
	}
	return o
}

// ensureWorker starts a worker unless one owns the data dir or systemd does
// (DESIGN.md §1.1); it stops the one it spawned when the daemon stops.
func (d *daemonProcess) ensureWorker(ctx context.Context, self string) error {
	home := d.c.forgeHome
	if d.c.getenv("INVOCATION_ID") != "" {
		if out, err := exec.CommandContext(ctx, "systemctl", "--user", "start", "forge-worker").CombinedOutput(); err != nil {
			d.log.WarnContext(ctx, "systemctl start forge-worker", "error", err, "output", strings.TrimSpace(string(out)))
		}
		<-ctx.Done()
		return nil
	}
	lockPath := filepath.Join(home, "worker", "lock")
	if err := os.MkdirAll(filepath.Dir(lockPath), 0o700); err != nil {
		return err
	}
	f, err := os.OpenFile(lockPath, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return err
	}
	held := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB) != nil
	if err := f.Close(); err != nil {
		return err
	}
	if held {
		// A worker from before a restart still owns the data directory. Watch
		// it: if it dies without a successor (found by M3 smoke — a wedged
		// orphan held the lock and no worker existed at all), spawn one then.
		d.log.InfoContext(ctx, "a worker already owns the data directory; watching instead of spawning")
		t := time.NewTicker(30 * time.Second)
		defer t.Stop()
		for {
			select {
			case <-ctx.Done():
				return nil
			case <-t.C:
			}
			f, err := os.OpenFile(lockPath, os.O_RDWR, 0o600)
			if err != nil {
				continue
			}
			free := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB) == nil
			if err := f.Close(); err != nil {
				d.log.WarnContext(ctx, "close lock probe", "error", err)
			}
			if free {
				d.log.WarnContext(ctx, "the worker holding the data directory is gone; spawning a fresh one")
				break
			}
		}
	}
	stdio, err := os.OpenFile(filepath.Join(home, "logs", "worker.stdio.log"), os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
	if err != nil {
		return err
	}
	cmd := exec.Command(self, "worker", "start")
	cmd.Env = worker.PassthroughEnv(os.Environ(), append(logging.Environ(d.handler), "FORGE_HOME="+home)...)
	cmd.Stdout, cmd.Stderr = stdio, stdio
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
	if err := cmd.Start(); err != nil {
		return errors.Join(fmt.Errorf("spawn worker: %w", err), stdio.Close())
	}
	if err := stdio.Close(); err != nil {
		return err
	}
	d.state.WorkerPID = cmd.Process.Pid
	if err := controlplane.WriteState(home, d.state); err != nil {
		return err
	}
	d.log.InfoContext(ctx, "worker spawned", "pid", cmd.Process.Pid)
	<-ctx.Done()
	if err := cmd.Process.Signal(syscall.SIGTERM); err == nil {
		done := make(chan struct{})
		go func() {
			if werr := cmd.Wait(); werr != nil {
				d.log.DebugContext(context.WithoutCancel(ctx), "worker exit after stop", "error", werr)
			}
			close(done)
		}()
		select {
		case <-done:
		case <-time.After(30 * time.Second):
			d.log.WarnContext(context.WithoutCancel(ctx), "worker did not stop in 30 s; it keeps running")
		}
	}
	return nil
}

func runDaemonStop(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("daemon stop")
	force := fs.Bool("force", false, "SIGKILL instead of SIGTERM; reconcile cleans up later")
	keepWorker := fs.Bool("keep-worker", false, "leave the spawned worker running")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	_, _, code := c.resolveLogging(lf, "cli.daemon")
	if code >= 0 {
		return code
	}
	st, err := controlplane.ReadState(c.forgeHome)
	if err != nil {
		return c.fail("daemon stop", err)
	}
	if st == nil || !st.Alive() {
		fmt.Fprintln(c.stdout, "daemon is not running")
		return 0
	}
	sig := syscall.SIGTERM
	if *force {
		sig = syscall.SIGKILL
	}
	if err := syscall.Kill(st.PID, sig); err != nil && !errors.Is(err, syscall.ESRCH) {
		return c.fail("daemon stop", err)
	}
	if *keepWorker && st.WorkerPID != 0 {
		fmt.Fprintf(c.stdout, "leaving worker %d running\n", st.WorkerPID)
	}
	deadline := time.Now().Add(35 * time.Second)
	for st.Alive() && time.Now().Before(deadline) {
		time.Sleep(100 * time.Millisecond)
	}
	if st.Alive() {
		return c.fail("daemon stop", fmt.Errorf("daemon %d did not exit; try --force", st.PID))
	}
	if *keepWorker || st.WorkerPID == 0 {
		fmt.Fprintf(c.stdout, "daemon %d stopped\n", st.PID)
		return 0
	}
	fmt.Fprintf(c.stdout, "daemon %d stopped\n", st.PID)
	return 0
}

func runDaemonStatus(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("daemon status")
	asJSON := fs.Bool("json", false, "print daemon.json")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.resolveLogging(lf, "cli.daemon")
	if code >= 0 {
		return code
	}
	st, err := controlplane.ReadState(c.forgeHome)
	if err != nil {
		return c.fail("daemon status", err)
	}
	locked, err := controlplane.IsLocked(c.forgeHome)
	if err != nil {
		return c.fail("daemon status", err)
	}
	if *asJSON {
		c.printJSON(map[string]any{"state": st, "lock_held": locked, "alive": st.Alive()})
		return 0
	}
	switch {
	case st == nil && !locked:
		fmt.Fprintln(c.stdout, "daemon: not running")
		return 1
	case st != nil && st.Alive():
		cl := c.client(log)
		cl.noAutoStart, cl.tolerateMismatch = true, true
		hs := "socket not answering"
		if err := cl.connect(ctx); err == nil {
			hs = "socket ok"
		}
		fmt.Fprintf(c.stdout, "daemon: running (pid %d, %s, %s, worker pid %d, since %s, %s)\n", st.PID, st.Version, st.State, st.WorkerPID, st.StartedAt.Local().Format(time.RFC3339), hs)
		return 0
	default:
		pid := 0
		if st != nil {
			pid = st.PID
		}
		fmt.Fprintf(c.stdout, "daemon: stale state (pid %d not running, lock held: %v)\n", pid, locked)
		return 1
	}
}

func runDaemonLogs(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("daemon logs")
	n := fs.Int("n", 50, "lines to show")
	follow := fs.Bool("f", false, "keep printing as the daemon logs (Ctrl-C exits)")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	if _, _, code := c.resolveLogging(lf, "cli.daemon"); code >= 0 {
		return code
	}
	path := filepath.Join(c.forgeHome, "logs", "daemon.log")
	data, err := os.ReadFile(path)
	if err != nil {
		return c.fail("daemon logs", err)
	}
	lines := strings.Split(strings.TrimRight(string(data), "\n"), "\n")
	if len(lines) > *n {
		lines = lines[len(lines)-*n:]
	}
	for _, l := range lines {
		fmt.Fprintln(c.stdout, l)
	}
	if !*follow {
		return 0
	}
	if err := tailFile(ctx, c.stdout, path, int64(len(data))); err != nil {
		return c.fail("daemon logs", err)
	}
	return 0
}

// tailFile follows path from offset, polling its size every 500 ms and copying
// what appeared; a shrink (rotation) restarts from the top of the new file.
// It returns nil when ctx ends — Ctrl-C is how the human leaves.
func tailFile(ctx context.Context, out io.Writer, path string, offset int64) error {
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

func runDaemonLogLevel(ctx context.Context, c *cmdContext, args []string) int {
	fs, lf := c.flags("daemon log-level")
	if code := c.parse(fs, args); code >= 0 {
		return code
	}
	_, log, code := c.resolveLogging(lf, "cli.daemon")
	if code >= 0 {
		return code
	}
	cl := c.client(log)
	cl.noAutoStart = true
	if err := cl.connect(ctx); err != nil {
		return c.fail("daemon log-level", err)
	}
	var out struct {
		Levels string `json:"levels"`
	}
	if fs.NArg() == 0 {
		if err := cl.do(ctx, http.MethodGet, "/api/v1/log-level", nil, &out); err != nil {
			return c.fail("daemon log-level", err)
		}
		fmt.Fprintln(c.stdout, out.Levels)
		return 0
	}
	if err := cl.do(ctx, http.MethodPost, "/api/v1/log-level", map[string]string{"levels": fs.Arg(0)}, &out); err != nil {
		return c.fail("daemon log-level", err)
	}
	fmt.Fprintln(c.stdout, out.Levels)
	return 0
}

// kbReindexLoop keeps the kb index in step with the files: on start, every five
// minutes, and whenever the API pings /api/v1/kb/reindex (the server closes over
// the same function; the timer covers edits made outside Forge).
func (d *daemonProcess) kbReindexLoop(ctx context.Context, st *store.Store) {
	log := d.handler.For("daemon.kb")
	reindex := func() {
		notes, findings, err := kb.Scan(d.cfg.KB.Path)
		if err != nil {
			log.WarnContext(ctx, "kb scan", "error", err)
			return
		}
		// Repo-scoped notes: each registered repository's .forge/notes is part
		// of the index (the per-repo .forge/ directory is the repo's Forge home).
		if repos, rerr := st.Repositories(ctx); rerr == nil {
			for _, r := range repos {
				rn, rf, rerr := kb.Scan(filepath.Join(r.Path, ".forge", "notes"))
				if rerr != nil {
					log.WarnContext(ctx, "repo kb scan", "repository", r.Name, "error", rerr)
					continue
				}
				notes = append(notes, rn...)
				findings = append(findings, rf...)
			}
		}
		for _, f := range findings {
			log.WarnContext(ctx, "kb note skipped", "path", f.Path, "problem", f.Problem)
		}
		var n int
		err = st.Write(ctx, func(tx *store.Tx) error {
			var werr error
			n, werr = tx.ReindexKb(ctx, notes)
			return werr
		})
		if err != nil {
			log.WarnContext(ctx, "kb reindex", "error", err)
			return
		}
		if n > 0 {
			log.InfoContext(ctx, "kb reindexed", "notes", n, "total", len(notes))
		}
	}
	reindex()
	t := time.NewTicker(5 * time.Minute)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			reindex()
		}
	}
}

// nightlyPrune applies the retention policy once a day; `forge prune` is the
// manual form of the same function.
func (d *daemonProcess) nightlyPrune(ctx context.Context, st *store.Store) {
	log := d.handler.For("daemon.prune")
	t := time.NewTicker(24 * time.Hour)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			wcfg, err := worker.LoadConfig(filepath.Join(d.c.forgeHome, "worker.toml"))
			if err != nil {
				log.WarnContext(ctx, "nightly prune: worker config", "error", err)
				continue
			}
			rep, err := controlplane.Prune(ctx, controlplane.PruneInput{DataDir: wcfg.DataDir, Retention: d.cfg.Retention, Now: time.Now(), Delete: true, Logger: log})
			if err != nil {
				log.WarnContext(ctx, "nightly prune", "error", err)
				continue
			}
			if err := st.Write(ctx, func(tx *store.Tx) error {
				return tx.Journal(ctx, "daemon.pruned", store.EntityDaemon, "daemon", rep)
			}); err != nil {
				log.WarnContext(ctx, "journal prune", "error", err)
			}
		}
	}
}
