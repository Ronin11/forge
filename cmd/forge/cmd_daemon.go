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
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"golang.org/x/sync/errgroup"

	"forge/internal/controlplane"
	"forge/internal/core/kb"
	"forge/internal/core/logging"
	"forge/internal/core/model"
	"forge/internal/core/plugin"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
	"forge/internal/core/worker"
	"forge/internal/integrator"
	"forge/internal/modes"
	"forge/internal/modes/all"
	"forge/internal/tools"
)

func runDaemon(ctx context.Context, c *cmdContext, args []string) int {
	if len(args) == 0 || strings.HasPrefix(args[0], "-") {
		// --help on a parent command is a request, not a mistake.
		code := 2
		if len(args) > 0 && (args[0] == "--help" || args[0] == "-h") {
			code = 0
		}
		fmt.Fprintln(c.stderr, "usage: forge daemon start|stop|restart|status|logs|log-level|rollback [flags]")
		return code
	}
	switch args[0] {
	case "rollback":
		return runDaemonRollback(ctx, c, args[1:])
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
		if _, serr := os.Stat(filepath.Join(home, prevDirName, prevBinName)); serr == nil {
			fmt.Fprintln(c.stderr, "forge daemon: a last-known-good binary and database snapshot exist; if this version keeps failing before serving, run 'forge daemon rollback'")
		}
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
	// stopPlugins reaps the plugin children (set once they start); execRestart
	// calls it before syscall.Exec so plugins never orphan across the restart.
	stopPlugins func()
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
	// The lock must NOT leak into children: the worker inherits every
	// non-CLOEXEC descriptor, and an inherited lock fd keeps the flock held
	// after a daemon kill -9, wedging auto-start until the worker exits
	// (found by M6 smoke 6). Set FD_CLOEXEC here — the fd may arrive from
	// the CLI hand-off or a previous exec without it — and clear it only at
	// exec time, in execRestart, where surviving the exec is the point.
	if err := setCloexec(d.lock.File().Fd(), true); err != nil {
		return fmt.Errorf("set cloexec on lock: %w", err)
	}
	ctx, stop := signal.NotifyContext(ctx, syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	st, err := store.Open(ctx, filepath.Join(home, controlplane.DBFile), store.Options{Logger: d.handler.For("store")})
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, st.Close()) }()
	runSup := newRunSupervisor(home, controlplaneRunConfig{portMin: d.cfg.Run.PortMin, portMax: d.cfg.Run.PortMax}, st, d.handler.For("run"), time.Now)
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
	// The errgroup context is created before the server so plugin processes
	// (and their supervision goroutines) are bound to the daemon's lifetime,
	// and tools plugins can register their tools before the registry freezes
	// at NewServer.
	g, gctx := errgroup.WithContext(ctx)
	sup, startPlugin, err := d.startPlugins(gctx, st, toolRegistry)
	if err != nil {
		return err
	}
	d.stopPlugins = sup.Shutdown
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
		ExecRestart:  func(execPath string) error { return d.execRestart(execPath, unixL, tcpL) },
		RegisterRepo: d.registerRepoOnTheFly,
		AddRepo:      d.addRepo,
		ArchiveRepo:  d.archiveRepo,
		RestoreRepo:  d.restoreRepo,
		StartApp:     runSup.StartApp,
		StopApp:      runSup.StopApp,
		RebuildApp:   runSup.RebuildApp,
		AppStatus:    runSup.AppStatus,
		ModelCall:    d.modelCall,
		Attention:    d.cfg.Attention,
		QuietHours:   d.cfg.Budget.QuietHours,
		Supervision:  d.cfg.Supervision,
		Store:        st, Policy: policy, Logger: d.handler.For("controlplane.http"), Version: version, Token: token, Home: home, Modes: registry,
		// Executable seeds auto-eval's walk to the checkout's evals/ + fixtures
		// (autoeval.go); when the binary is not in its checkout, auto-eval
		// stays disabled and approvals use the force override.
		Executable:    self,
		RequiredLevel: func(string) int { return 1 },
		AllowHosts:    d.cfg.Sandbox.AllowHosts,
		// Attempt worktrees get conflict-resistant git options through the
		// claim's policy env (STYLE.md §10) — never a .git/config write.
		GitConfig:     map[string]string{"merge.conflictstyle": "zdiff3", "rerere.enabled": "true"},
		MaxStackDepth: d.cfg.Integration.MaxStackDepth,
		KbDir:         d.cfg.KB.Path,
		Tools:         toolRegistry,
		// M10 routing (DESIGN.md §21): the model table, alias set, routing
		// policy, and per-runner capacities from the loaded config.
		ModelInfo:        d.cfg.ModelInfoFor,
		ModelAliases:     d.cfg.ModelAliases(),
		Routing:          d.cfg.Routing,
		RunnerCapacities: runnerCapacities(d.cfg),
		PluginHealth:     sup.Health,
		PluginStart:      startPlugin,
		PluginStop:       func(name string) { sup.Stop(name) },
		// The same discovery roots the supervisor used, so install resolves an
		// out-of-tree plugin by name (warnings already logged in startPlugins).
		PluginRoots: d.cfg.PluginRoots(home, nil),
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
	// Bring back apps that were running before a restart (daemon-supervised
	// children die with the daemon; the intent file records what to relaunch).
	go runSup.reconcile(ctx, func(name string) (string, bool) {
		r, rerr := st.Repository(ctx, name)
		if rerr != nil || r.Archived {
			return "", false
		}
		return r.Path, true
	})
	ui, err := controlplane.NewUI(st, d.handler.For("controlplane.ui"), nil)
	if err != nil {
		return err
	}
	ui.SetPluginHealth(sup.Health)
	ui.SetAttention(d.cfg.Attention, d.cfg.Budget.QuietHours)
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
	// Post-start self-check (§23): bootstrap ran and the listeners are bound;
	// prove the store answers before claiming health. Only then refresh the
	// last-known-good snapshot — a failure to record it degrades rollback,
	// not the daemon.
	if err := d.selfCheck(ctx, st); err != nil {
		return err
	}
	if err := captureLastKnownGood(ctx, st, home, self); err != nil {
		d.log.WarnContext(ctx, "record last known good", "error", err)
	}

	g.Go(func() error { return srv.Serve(gctx, unixL, tcpL) })
	g.Go(func() error { <-gctx.Done(); sup.Wait(); return nil })
	g.Go(func() error { srv.RunSweeper(gctx, 10*time.Second, d.cfg.Reflection); return nil })
	g.Go(func() error { srv.RunAttention(gctx, time.Minute); return nil })
	g.Go(func() error { srv.RunSupervision(gctx, 45*time.Second); return nil })
	integ := integrator.New(st, d.handler.For("integrator"), time.Now, integrator.Config{Home: home, MaxRebaseAttempts: d.cfg.Integration.MaxRebaseAttempts})
	g.Go(func() error { integ.Run(gctx); return nil })
	g.Go(func() error { d.kbReindexLoop(gctx, st); return nil })
	g.Go(func() error { d.nightlyPrune(gctx, st); return nil })
	g.Go(func() error { d.nightlyBackup(gctx, st); return nil })
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

// startPlugins brings the plugin runtime up (DESIGN.md §17): discover
// manifests under <home>/plugins (first-party repo directories are install
// sources, never run sources), start every enabled plugin under the
// supervisor with a freshly minted token — the plain token lives only in the
// child's environment; only its hash rests in the store, refreshed here so a
// daemon restart invalidates old tokens — and bridge tools plugins into the
// registry before it freezes at NewServer. It returns the supervisor and the
// hook the enable handler uses to (re)start one plugin at runtime.
func (d *daemonProcess) startPlugins(ctx context.Context, st *store.Store, reg *tools.Registry) (*plugin.Supervisor, func(name, token string) error, error) {
	home := d.c.forgeHome
	logsDir := filepath.Join(home, "logs", "plugins")
	if err := os.MkdirAll(logsDir, 0o700); err != nil {
		return nil, nil, fmt.Errorf("create plugin logs dir: %w", err)
	}
	log := d.handler.For("daemon.plugins")
	sup := plugin.NewSupervisor(plugin.SupervisorOptions{LogFor: d.handler.For})
	// bridges is filled here, before the server exists, and only read by the
	// runtime start hook afterwards.
	bridges := map[string]*controlplane.PluginTools{}
	launch := func(ctx context.Context, m *plugin.Manifest, token string) error {
		f, err := os.OpenFile(filepath.Join(logsDir, m.Name+".log"), os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
		if err != nil {
			return fmt.Errorf("open plugin log: %w", err)
		}
		env := worker.PassthroughEnv(os.Environ(), append(logging.Environ(d.handler),
			"FORGE_SOCKET="+filepath.Join(home, controlplane.SocketFile),
			"FORGE_TOKEN="+token,
			"FORGE_PLUGIN_DIR="+m.Dir)...)
		spec := plugin.Spec{Manifest: m, Env: env, LogSink: f}
		if m.Has(plugin.CapTools) {
			if b := bridges[m.Name]; b != nil {
				spec.OnStdio = b.Attach
			}
		}
		return sup.Start(ctx, spec)
	}
	// Discover across the built-in root and every configured plugin_dir so an
	// out-of-tree plugin (e.g. ~/.config/forge/plugins/foo) is a first-class
	// citizen (DESIGN.md §17). A missing configured root warns, never fails.
	roots := d.cfg.PluginRoots(home, func(dir string, err error) {
		log.WarnContext(ctx, "configured plugin_dir missing or unreadable; skipped", "dir", dir, "error", err)
	})
	manifests := plugin.Discover(roots, func(dir string, err error) {
		log.WarnContext(ctx, "invalid plugin manifest", "dir", dir, "error", err)
	})
	byName := map[string]*plugin.Manifest{}
	for _, m := range manifests {
		byName[m.Name] = m
	}
	rows, err := st.Plugins(ctx)
	if err != nil {
		return nil, nil, err
	}
	for _, row := range rows {
		if !row.Enabled {
			continue
		}
		m := byName[row.Name]
		if m == nil {
			log.WarnContext(ctx, "enabled plugin has no manifest under <home>/plugins; not started", "plugin", row.Name)
			continue
		}
		token, hash, err := plugin.NewToken()
		if err != nil {
			return nil, nil, err
		}
		if err := st.Write(ctx, func(tx *store.Tx) error {
			_, werr := tx.EnablePlugin(ctx, row.Name, hash)
			return werr
		}); err != nil {
			return nil, nil, fmt.Errorf("refresh plugin token: %w", err)
		}
		if m.Has(plugin.CapTools) {
			bridges[m.Name] = controlplane.NewPluginTools(m.Name, d.handler.For("plugin."+m.Name))
		}
		if err := launch(ctx, m, token); err != nil {
			log.WarnContext(ctx, "plugin did not start", "plugin", m.Name, "error", err)
		}
	}
	for name, b := range bridges {
		wctx, cancel := context.WithTimeout(ctx, 10*time.Second)
		err := controlplane.RegisterPluginTools(wctx, reg, b)
		cancel()
		if err != nil {
			log.WarnContext(ctx, "plugin tools not registered", "plugin", name, "error", err)
		}
	}
	startPlugin := func(name, token string) error {
		// Enable can name a plugin under any discovery root, so search them in
		// the same order (earlier root wins) rather than assuming <home>/plugins.
		m, err := plugin.LoadFromRoots(roots, name)
		if err != nil {
			return err
		}
		return launch(ctx, m, token)
	}
	return sup, startPlugin, nil
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

// registerRepoOnTheFly backs DESIGN §1.3: resolve --repo X as an absolute
// checkout path or <projects_root>/X, validate it is a git checkout with an
// origin, append it to worker.toml (the daemon owns bootstrap's files), and
// hand back the entry for a provisional store row. The worker re-reads
// worker.toml on its next registration tick and advertises it.
func (d *daemonProcess) registerRepoOnTheFly(ctx context.Context, nameOrPath string) (protocol.Repository, error) {
	var name, path string
	if filepath.IsAbs(nameOrPath) {
		path = filepath.Clean(nameOrPath)
		name = filepath.Base(path)
	} else {
		if err := model.ValidateName(nameOrPath); err != nil {
			return protocol.Repository{}, err
		}
		name = nameOrPath
		path = filepath.Join(d.cfg.Repositories.ProjectsRoot, name)
	}
	if err := model.ValidateName(name); err != nil {
		return protocol.Repository{}, err
	}
	vctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	r, err := (worker.Git{}).ValidateRepository(vctx, name, path, "")
	if err != nil {
		return protocol.Repository{}, err
	}
	// A checkout without refs/remotes/origin/HEAD (common) leaves resolve_base
	// with nothing to fall back to (M6 smoke 7), so detect the base branch
	// now: origin/HEAD when set, else the checkout's current branch.
	base := detectBaseBranch(vctx, r.Path)
	if err := worker.AddRepository(filepath.Join(d.c.forgeHome, "worker.toml"), name, r.Path, base); err != nil {
		return protocol.Repository{}, err
	}
	return protocol.Repository{Name: name, Path: r.Path, OriginIdentity: r.OriginIdentity, BaseBranch: base, Project: "default"}, nil
}

// addRepo backs the Repos page's "+" (POST /api/v1/repositories): a remote URL
// is cloned into ProjectsRoot/<name> first, a local path (or bare name) is
// registered as-is. Either way it ends in registerRepoOnTheFly, so the worker
// picks it up on its next refresh.
func (d *daemonProcess) addRepo(ctx context.Context, path, url, name string) (protocol.Repository, error) {
	if url == "" {
		if path == "" {
			return protocol.Repository{}, fmt.Errorf("a local path or a remote URL is required")
		}
		return d.registerRepoOnTheFly(ctx, path)
	}
	n := name
	if n == "" {
		n = repoNameFromURL(url)
	}
	if err := model.ValidateName(n); err != nil {
		return protocol.Repository{}, fmt.Errorf("repository name %q (from %q): %w", n, url, err)
	}
	dest := filepath.Join(d.cfg.Repositories.ProjectsRoot, n)
	if _, err := os.Stat(dest); err == nil {
		return protocol.Repository{}, fmt.Errorf("%s already exists — pick a different name or add it by path", dest)
	}
	cctx, cancel := context.WithTimeout(ctx, 10*time.Minute)
	defer cancel()
	if out, err := exec.CommandContext(cctx, "git", "clone", url, dest).CombinedOutput(); err != nil {
		return protocol.Repository{}, fmt.Errorf("clone %s: %w: %s", url, err, strings.TrimSpace(string(out)))
	}
	return d.registerRepoOnTheFly(ctx, dest)
}

// archiveRepo captures the checkout's clone URL (for a later Restore), drops the
// repository from worker.toml so the worker stops advertising it, then deletes
// the checkout to free disk. The store row and its metadata are kept by the
// caller. Returns the captured URL.
func (d *daemonProcess) archiveRepo(ctx context.Context, name, path string) (string, error) {
	originURL := ""
	if out, err := exec.CommandContext(ctx, "git", "-C", path, "remote", "get-url", "origin").Output(); err == nil {
		originURL = strings.TrimSpace(string(out))
	}
	if err := worker.RemoveRepository(filepath.Join(d.c.forgeHome, "worker.toml"), name); err != nil {
		return "", err
	}
	if err := safeRemoveCheckout(path); err != nil {
		return "", err
	}
	return originURL, nil
}

// restoreRepo re-clones an archived repository from its saved URL back into
// ProjectsRoot/<name> and re-registers it.
func (d *daemonProcess) restoreRepo(ctx context.Context, name, originURL string) (protocol.Repository, error) {
	if originURL == "" {
		return protocol.Repository{}, fmt.Errorf("repository %s has no saved origin URL to restore from", name)
	}
	dest := filepath.Join(d.cfg.Repositories.ProjectsRoot, name)
	if _, err := os.Stat(dest); err == nil {
		return protocol.Repository{}, fmt.Errorf("%s already exists on disk", dest)
	}
	cctx, cancel := context.WithTimeout(ctx, 10*time.Minute)
	defer cancel()
	if out, err := exec.CommandContext(cctx, "git", "clone", originURL, dest).CombinedOutput(); err != nil {
		return protocol.Repository{}, fmt.Errorf("clone %s: %w: %s", originURL, err, strings.TrimSpace(string(out)))
	}
	return d.registerRepoOnTheFly(ctx, dest)
}

// repoNameFromURL derives a repository name from a clone URL: the last path
// segment with any .git suffix and trailing slash removed.
func repoNameFromURL(url string) string {
	u := strings.TrimRight(strings.TrimSpace(url), "/")
	u = strings.TrimSuffix(u, ".git")
	if i := strings.LastIndexAny(u, "/:"); i >= 0 {
		u = u[i+1:]
	}
	return u
}

// safeRemoveCheckout deletes a repository checkout, refusing paths that would be
// catastrophic to rm -rf: a non-absolute path, a symlink, a filesystem root or
// a home directory, or anything that is not itself a git checkout (has .git).
func safeRemoveCheckout(path string) error {
	if !filepath.IsAbs(path) {
		return fmt.Errorf("refuse to delete non-absolute checkout path %q", path)
	}
	clean := filepath.Clean(path)
	if clean == "/" || clean == filepath.Dir(clean) {
		return fmt.Errorf("refuse to delete %q", clean)
	}
	if home, err := os.UserHomeDir(); err == nil && clean == filepath.Clean(home) {
		return fmt.Errorf("refuse to delete home directory %q", clean)
	}
	fi, err := os.Lstat(clean)
	if os.IsNotExist(err) {
		return nil // already gone (e.g. a manually deleted checkout) — nothing to remove
	}
	if err != nil {
		return fmt.Errorf("stat checkout %s: %w", clean, err)
	}
	if fi.Mode()&os.ModeSymlink != 0 {
		return fmt.Errorf("refuse to delete symlinked checkout %q", clean)
	}
	if !fi.IsDir() {
		return fmt.Errorf("checkout %q is not a directory", clean)
	}
	if _, err := os.Stat(filepath.Join(clean, ".git")); err != nil {
		return fmt.Errorf("refuse to delete %q: not a git checkout (no .git)", clean)
	}
	return os.RemoveAll(clean)
}

// detectBaseBranch names the branch attempts on an on-the-fly repository
// resolve against: origin's HEAD when the clone recorded it, else whatever
// branch the checkout is on; "" lets resolve_base try its own fallbacks.
func detectBaseBranch(ctx context.Context, path string) string {
	if out, err := exec.CommandContext(ctx, "git", "-C", path, "symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD").Output(); err == nil {
		return strings.TrimPrefix(strings.TrimSpace(string(out)), "origin/")
	}
	if out, err := exec.CommandContext(ctx, "git", "-C", path, "symbolic-ref", "--quiet", "--short", "HEAD").Output(); err == nil {
		return strings.TrimSpace(string(out))
	}
	return ""
}

// modelCall runs one cheap headless claude completion for the concierge
// (handlers_assistant.go): the system prompt as --append-system-prompt, the user
// text on stdin, and the model's text pulled from --output-format json's result.
func (d *daemonProcess) modelCall(ctx context.Context, system, user, model string) (string, error) {
	bin, err := resolveClaude()
	if err != nil {
		return "", err
	}
	cctx, cancel := context.WithTimeout(ctx, 60*time.Second)
	defer cancel()
	cmd := exec.CommandContext(cctx, bin, "--print", "--output-format", "json", "--model", model, "--append-system-prompt", system)
	cmd.Stdin = strings.NewReader(user)
	var out, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &out, &stderr
	if err := cmd.Run(); err != nil {
		return "", fmt.Errorf("claude: %w: %s", err, strings.TrimSpace(stderr.String()))
	}
	var res struct {
		Result string `json:"result"`
	}
	if err := json.Unmarshal(out.Bytes(), &res); err != nil {
		return "", fmt.Errorf("parse claude output: %w", err)
	}
	return res.Result, nil
}

// resolveClaude finds the claude binary: PATH first, then the common install
// locations (the daemon's systemd env may not carry the mise shims).
func resolveClaude() (string, error) {
	if p, err := exec.LookPath("claude"); err == nil {
		return p, nil
	}
	home, herr := os.UserHomeDir()
	if herr != nil {
		home = ""
	}
	for _, c := range []string{
		filepath.Join(home, ".local/share/mise/installs/claude/latest/claude"),
		filepath.Join(home, ".local/bin/claude"),
		"/usr/local/bin/claude",
	} {
		if fi, err := os.Stat(c); err == nil && !fi.IsDir() {
			return c, nil
		}
	}
	return "", fmt.Errorf("claude binary not found on PATH or common locations")
}

// runnerCapacities extracts each runner's capacity for the scheduler's
// runner-slot dimension (DESIGN.md §21); a capacity ≤ 0 stays unbounded.
func runnerCapacities(cfg *controlplane.Config) map[string]int {
	out := map[string]int{}
	for name, r := range cfg.Runners {
		if r.Capacity > 0 {
			out[name] = r.Capacity
		}
	}
	return out
}
