package main

// The run supervisor drives a repository's app lifecycle with no agent in the
// loop: the Repos page's Start / Stop / Rebuild run the commands a repo declares
// in [run] of its .forge/config.toml. Each app is a daemon-supervised child in
// its own process group; the daemon leases it a free port (injected as PortEnv),
// captures its output to ~/.forge/run/<repo>.log, and treats it as up once
// ReadyLog appears (or a health check passes, or a short grace elapses). State
// lives in memory; a small intent file lets apps that were running come back up
// when the daemon restarts.

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"forge/internal/core/protocol"
	"forge/internal/store"
	"forge/internal/worker"
)

// runSupervisor tracks every running app. It is the daemon's, wired into the
// server as the App* hooks.
type runSupervisor struct {
	mu        sync.Mutex
	procs     map[string]*appProc
	cfg       controlplaneRunConfig
	forgeHome string
	store     *store.Store
	log       *slog.Logger
	now       func() time.Time
}

// controlplaneRunConfig is the port range, copied so the supervisor does not
// import the controlplane package.
type controlplaneRunConfig struct{ portMin, portMax int }

// appProc is one supervised app.
type appProc struct {
	name      string
	cmd       *exec.Cmd
	port      int
	state     string // building | starting | running | stopped | errored
	startedAt time.Time
	logPath   string
	hotReload bool
	message   string
	stopping  bool // Stop() set this, so the exit watcher does not mark errored
}

func newRunSupervisor(home string, cfg controlplaneRunConfig, st *store.Store, log *slog.Logger, now func() time.Time) *runSupervisor {
	if cfg.portMin <= 0 {
		cfg.portMin = 3000
	}
	if cfg.portMax < cfg.portMin {
		cfg.portMax = cfg.portMin + 99
	}
	return &runSupervisor{procs: map[string]*appProc{}, cfg: cfg, forgeHome: home, store: st, log: log, now: now}
}

// repoRun reads a repository's [run] config from its checkout.
func repoRun(repoPath string) (worker.RepoRun, error) {
	ft, err := worker.ReadForgeToml(repoPath)
	if err != nil {
		return worker.RepoRun{}, err
	}
	if ft == nil {
		return worker.RepoRun{}, nil
	}
	return ft.Run, nil
}

// AppStatus reports a repository's run state, reading its config so a stopped
// repo still shows whether it is Configured and its HotReload flag.
func (s *runSupervisor) AppStatus(_ context.Context, name, repoPath string) (protocol.AppStatus, error) {
	run, cfgErr := repoRun(repoPath)
	if cfgErr != nil {
		run = worker.RepoRun{}
	}
	st := protocol.AppStatus{Configured: len(run.Start) > 0, State: "stopped", HotReload: run.HotReload}
	s.mu.Lock()
	defer s.mu.Unlock()
	if p := s.procs[name]; p != nil {
		st.State, st.Port, st.StartedAt, st.LogPath, st.Message = p.state, p.port, p.startedAt, p.logPath, p.message
		st.HotReload = p.hotReload
		if p.cmd != nil && p.cmd.Process != nil {
			st.PID = p.cmd.Process.Pid
		}
		if p.port > 0 && (p.state == "running" || p.state == "starting") {
			st.URL = fmt.Sprintf("http://localhost:%d", p.port)
		}
	}
	return st, nil
}

// StartApp launches the repository's [run] start command on a leased port.
func (s *runSupervisor) StartApp(ctx context.Context, name, repoPath string) (protocol.AppStatus, error) {
	run, err := repoRun(repoPath)
	if err != nil {
		return protocol.AppStatus{}, err
	}
	if len(run.Start) == 0 {
		return protocol.AppStatus{}, fmt.Errorf("repository %s declares no [run] start command in .forge/config.toml", name)
	}
	s.mu.Lock()
	if p := s.procs[name]; p != nil && (p.state == "running" || p.state == "starting" || p.state == "building") {
		s.mu.Unlock()
		return protocol.AppStatus{}, fmt.Errorf("repository %s is already %s", name, p.state)
	}
	port, err := s.leasePort()
	if err != nil {
		s.mu.Unlock()
		return protocol.AppStatus{}, err
	}
	logPath, logf, err := s.openLog(name)
	if err != nil {
		s.mu.Unlock()
		return protocol.AppStatus{}, err
	}
	cmd := exec.Command(run.Start[0], run.Start[1:]...)
	cmd.Dir = repoPath
	cmd.Env = appEnv(run, port)
	cmd.Stdout, cmd.Stderr = logf, logf
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := cmd.Start(); err != nil {
		if cerr := logf.Close(); cerr != nil {
			s.log.WarnContext(ctx, "close app log handle", "err", cerr)
		}
		s.mu.Unlock()
		return protocol.AppStatus{}, fmt.Errorf("start %s: %w", name, err)
	}
	p := &appProc{name: name, cmd: cmd, port: port, state: "starting", startedAt: s.now(), logPath: logPath, hotReload: run.HotReload}
	s.procs[name] = p
	s.mu.Unlock()
	if cerr := logf.Close(); cerr != nil { // the child holds its own fd
		s.log.WarnContext(ctx, "close app log handle", "repository", name, "err", cerr)
	}

	s.log.InfoContext(ctx, "app started", "repository", name, "port", port, "pid", cmd.Process.Pid)
	s.writeIntent()
	go s.watchExit(p)
	go s.watchReady(p, run)
	return s.AppStatus(ctx, name, repoPath)
}

// StopApp stops a running app: its declared stop command if any, else a signal
// to the whole process group.
func (s *runSupervisor) StopApp(ctx context.Context, name, repoPath string) (protocol.AppStatus, error) {
	s.mu.Lock()
	p := s.procs[name]
	if p == nil || p.cmd == nil || p.cmd.Process == nil {
		s.mu.Unlock()
		return s.AppStatus(ctx, name, repoPath)
	}
	p.stopping = true
	pid := p.cmd.Process.Pid
	run, cfgErr := repoRun(repoPath)
	if cfgErr != nil {
		run = worker.RepoRun{}
	}
	s.mu.Unlock()

	if len(run.Stop) > 0 {
		c := exec.Command(run.Stop[0], run.Stop[1:]...)
		c.Dir = repoPath
		c.Env = appEnv(run, p.port)
		if out, err := c.CombinedOutput(); err != nil {
			s.log.WarnContext(ctx, "app stop command failed; signalling group", "repository", name, "err", err, "out", strings.TrimSpace(string(out)))
		}
	}
	// Signal the whole group either way; a graceful stop should have exited.
	if err := syscall.Kill(-pid, syscall.SIGTERM); err != nil {
		s.log.WarnContext(ctx, "signal app group", "repository", name, "err", err)
	}
	go func() {
		time.Sleep(5 * time.Second)
		if err := syscall.Kill(-pid, syscall.SIGKILL); err != nil {
			return
		}
	}()

	s.mu.Lock()
	if s.procs[name] == p {
		p.state = "stopped"
	}
	s.mu.Unlock()
	s.clearAppURL(ctx, name)
	s.writeIntent()
	s.log.InfoContext(ctx, "app stopped", "repository", name)
	return s.AppStatus(ctx, name, repoPath)
}

// RebuildApp runs the build command (blocking, captured to the log). When the
// app is running and not hot-reloading, it is restarted afterwards.
func (s *runSupervisor) RebuildApp(ctx context.Context, name, repoPath string) (protocol.AppStatus, error) {
	run, err := repoRun(repoPath)
	if err != nil {
		return protocol.AppStatus{}, err
	}
	if len(run.Build) == 0 {
		return protocol.AppStatus{}, fmt.Errorf("repository %s declares no [run] build command", name)
	}
	s.mu.Lock()
	running := false
	if p := s.procs[name]; p != nil && p.state == "running" {
		running = true
		p.state = "building"
	}
	s.mu.Unlock()

	_, logf, err := s.openLog(name)
	if err != nil {
		return protocol.AppStatus{}, err
	}
	fmt.Fprintf(logf, "\n=== forge rebuild %s ===\n", s.now().Format(time.RFC3339))
	c := exec.CommandContext(ctx, run.Build[0], run.Build[1:]...)
	c.Dir = repoPath
	c.Env = appEnv(run, 0)
	c.Stdout, c.Stderr = logf, logf
	buildErr := c.Run()
	if cerr := logf.Close(); cerr != nil {
		s.log.WarnContext(ctx, "close rebuild log handle", "repository", name, "err", cerr)
	}
	if buildErr != nil {
		s.mu.Lock()
		if p := s.procs[name]; p != nil && p.state == "building" {
			p.state, p.message = "errored", "build failed: "+buildErr.Error()
		}
		s.mu.Unlock()
		return protocol.AppStatus{}, fmt.Errorf("build %s failed (see the app log): %w", name, buildErr)
	}
	s.mu.Lock()
	if p := s.procs[name]; p != nil && p.state == "building" {
		p.state = "running"
	}
	s.mu.Unlock()
	// A non-hot-reload app must restart to pick up the new build.
	if running && !run.HotReload {
		if _, err := s.StopApp(ctx, name, repoPath); err != nil {
			return protocol.AppStatus{}, err
		}
		time.Sleep(500 * time.Millisecond)
		return s.StartApp(ctx, name, repoPath)
	}
	s.log.InfoContext(ctx, "app rebuilt", "repository", name, "restarted", running && !run.HotReload)
	return s.AppStatus(ctx, name, repoPath)
}

// watchExit reaps the child and records an unexpected exit as errored.
func (s *runSupervisor) watchExit(p *appProc) {
	err := p.cmd.Wait()
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.procs[p.name] != p {
		return // superseded by a newer start
	}
	if p.stopping {
		p.state = "stopped"
		return
	}
	p.state = "errored"
	if err != nil {
		p.message = "exited: " + err.Error()
	} else {
		p.message = "process exited"
	}
	s.log.Warn("app exited unexpectedly", "repository", p.name, "err", err)
	go s.clearAppURL(context.Background(), p.name)
}

// watchReady flips starting → running once ReadyLog appears, a health check
// passes, or a grace period elapses; it also sets the repository's app URL.
func (s *runSupervisor) watchReady(p *appProc, run worker.RepoRun) {
	timeout := time.Duration(run.ReadyTimeoutS) * time.Second
	if timeout <= 0 {
		timeout = 60 * time.Second
	}
	deadline := time.Now().Add(timeout)
	grace := time.Now().Add(1500 * time.Millisecond)
	for {
		s.mu.Lock()
		cur := s.procs[p.name]
		state := ""
		if cur == p {
			state = p.state
		}
		s.mu.Unlock()
		if cur != p || state != "starting" {
			return // stopped, exited, or already running
		}
		ready := false
		switch {
		case run.ReadyLog != "" && logContains(p.logPath, run.ReadyLog):
			ready = true
		case run.HealthPath != "" && healthOK(p.port, run.HealthPath):
			ready = true
		case run.ReadyLog == "" && run.HealthPath == "" && time.Now().After(grace):
			ready = true
		case time.Now().After(deadline):
			ready = true // optimistic: still alive at the deadline
		}
		if ready {
			s.mu.Lock()
			if s.procs[p.name] == p && p.state == "starting" {
				p.state = "running"
				if time.Now().After(deadline) {
					p.message = "ready not confirmed; assuming up"
				}
			}
			s.mu.Unlock()
			s.setAppURL(context.Background(), p.name, fmt.Sprintf("http://localhost:%d", p.port))
			s.log.Info("app ready", "repository", p.name, "port", p.port)
			return
		}
		time.Sleep(400 * time.Millisecond)
	}
}

// leasePort returns a free port in the configured range not already leased.
// Caller holds s.mu.
func (s *runSupervisor) leasePort() (int, error) {
	used := map[int]bool{}
	for _, p := range s.procs {
		if p.port > 0 && p.state != "stopped" && p.state != "errored" {
			used[p.port] = true
		}
	}
	for port := s.cfg.portMin; port <= s.cfg.portMax; port++ {
		if used[port] {
			continue
		}
		l, err := net.Listen("tcp", fmt.Sprintf("127.0.0.1:%d", port))
		if err != nil {
			continue // in use by something else
		}
		if cerr := l.Close(); cerr != nil {
			continue
		}
		return port, nil
	}
	return 0, fmt.Errorf("no free port in %d-%d", s.cfg.portMin, s.cfg.portMax)
}

func (s *runSupervisor) openLog(name string) (string, *os.File, error) {
	dir := filepath.Join(s.forgeHome, "run")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", nil, err
	}
	path := filepath.Join(dir, name+".log")
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	return path, f, err
}

func (s *runSupervisor) setAppURL(ctx context.Context, name, url string) {
	if s.store == nil {
		return
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.SetRepositoryAppURL(ctx, name, url) }); err != nil {
		s.log.WarnContext(ctx, "set app url", "repository", name, "err", err)
	}
}

func (s *runSupervisor) clearAppURL(ctx context.Context, name string) {
	s.setAppURL(ctx, name, "")
}

// appEnv is the app's environment: the daemon's, plus the [run] env, plus the
// leased port injected as PortEnv (when port > 0 and PortEnv is set).
func appEnv(run worker.RepoRun, port int) []string {
	env := os.Environ()
	for k, v := range run.Env {
		env = append(env, k+"="+v)
	}
	if port > 0 && run.PortEnv != "" {
		env = append(env, fmt.Sprintf("%s=%d", run.PortEnv, port))
	}
	return env
}

func logContains(path, sub string) bool {
	b, err := os.ReadFile(path)
	if err != nil {
		return false
	}
	return strings.Contains(string(b), sub)
}

func healthOK(port int, path string) bool {
	if !strings.HasPrefix(path, "/") {
		path = "/" + path
	}
	c := http.Client{Timeout: 800 * time.Millisecond}
	resp, err := c.Get(fmt.Sprintf("http://localhost:%d%s", port, path))
	if err != nil {
		return false
	}
	ok := resp.StatusCode < 500
	if cerr := resp.Body.Close(); cerr != nil {
		return ok
	}
	return ok
}

// intentFile records which apps should be running, so a daemon restart can
// bring them back.
func (s *runSupervisor) intentPath() string { return filepath.Join(s.forgeHome, "run", "intent.json") }

func (s *runSupervisor) writeIntent() {
	s.mu.Lock()
	want := []string{}
	for name, p := range s.procs {
		if p.state == "running" || p.state == "starting" || p.state == "building" {
			want = append(want, name)
		}
	}
	s.mu.Unlock()
	if err := os.MkdirAll(filepath.Join(s.forgeHome, "run"), 0o755); err != nil {
		s.log.Warn("write run intent: mkdir", "err", err)
		return
	}
	b, err := json.Marshal(want)
	if err != nil {
		return
	}
	if err := os.WriteFile(s.intentPath(), b, 0o644); err != nil {
		s.log.Warn("write run intent", "err", err)
	}
}

// reconcile re-starts the apps the intent file says were running (daemon boot).
// repoPath resolves a repository name to its checkout.
func (s *runSupervisor) reconcile(ctx context.Context, repoPath func(name string) (string, bool)) {
	b, err := os.ReadFile(s.intentPath())
	if err != nil {
		return
	}
	var want []string
	if json.Unmarshal(b, &want) != nil {
		return
	}
	for _, name := range want {
		path, ok := repoPath(name)
		if !ok {
			continue
		}
		if _, err := s.StartApp(ctx, name, path); err != nil {
			s.log.WarnContext(ctx, "reconcile: restart app", "repository", name, "err", err)
		}
	}
}
