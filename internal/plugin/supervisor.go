package plugin

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"log/slog"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
)

// Spec is one plugin the supervisor should keep running. The caller (the
// daemon) owns paths and secrets: it opens the log sink and builds the
// environment; the supervisor only runs the argv and reports health.
type Spec struct {
	Manifest *Manifest
	// Env is the child's whole environment, built by the daemon (the token,
	// socket path, and the §1.1 pass-through). The supervisor adds nothing.
	Env []string
	// LogSink receives the child's output line-by-line (stderr always;
	// stdout too unless OnStdio claims it). A sink that is an io.Closer is
	// closed when supervision of this plugin ends.
	LogSink io.Writer
	// OnStdio, when set (tools plugins), receives the new child's stdin and
	// stdout after every start; stdout then belongs to the callback (it is
	// the MCP channel), not the log. The callback owns closing stdin; stdout
	// reaches EOF when the child exits.
	OnStdio func(stdin io.WriteCloser, stdout io.ReadCloser)
}

// PluginHealth is one plugin's live state for doctor and the System page.
type PluginHealth struct {
	Name     string    `json:"name"`
	Running  bool      `json:"running"`
	PID      int       `json:"pid,omitempty"`
	Restarts int       `json:"restarts"`
	LastExit string    `json:"last_exit,omitempty"`
	Since    time.Time `json:"since,omitempty"`
}

// Backoff is the restart schedule: Initial doubling to Max between failed
// starts, reset to Initial after a run of at least ResetAfter.
type Backoff struct {
	Initial    time.Duration
	Max        time.Duration
	ResetAfter time.Duration
}

// defaults per DESIGN.md §17.
func (b Backoff) withDefaults() Backoff {
	if b.Initial <= 0 {
		b.Initial = time.Second
	}
	if b.Max <= 0 {
		b.Max = time.Minute
	}
	if b.ResetAfter <= 0 {
		b.ResetAfter = 5 * time.Minute
	}
	return b
}

// next is the pure backoff rule: the delay after an exit, given the previous
// delay and how long the process stayed up.
func (b Backoff) next(prev, uptime time.Duration) time.Duration {
	if uptime >= b.ResetAfter {
		return b.Initial
	}
	d := prev * 2
	if d > b.Max {
		d = b.Max
	}
	return d
}

// SupervisorOptions are the supervisor's injectable knobs; zero values take
// the production defaults.
type SupervisorOptions struct {
	// LogFor builds the per-plugin logger (component "plugin.<name>");
	// nil discards.
	LogFor    func(component string) *slog.Logger
	Backoff   Backoff
	KillGrace time.Duration // SIGTERM → SIGKILL grace; default 5s
}

// Supervisor keeps enabled plugins' processes running: start, restart per the
// manifest's Restart policy with exponential backoff, stop on context
// cancellation (SIGTERM, then SIGKILL after KillGrace). It knows nothing about
// tokens, files, or the store.
type Supervisor struct {
	logFor    func(string) *slog.Logger
	backoff   Backoff
	killGrace time.Duration

	// mu guards running and health.
	mu      sync.Mutex
	running map[string]*supervised
	health  map[string]*PluginHealth

	wg sync.WaitGroup
}

// supervised is one plugin's supervision goroutine.
type supervised struct {
	cancel context.CancelFunc
	done   chan struct{}
}

// NewSupervisor builds an empty supervisor; Start adds plugins.
func NewSupervisor(o SupervisorOptions) *Supervisor {
	if o.LogFor == nil {
		o.LogFor = func(string) *slog.Logger { return slog.New(slog.DiscardHandler) }
	}
	if o.KillGrace <= 0 {
		o.KillGrace = 5 * time.Second
	}
	return &Supervisor{
		logFor: o.LogFor, backoff: o.Backoff.withDefaults(), killGrace: o.KillGrace,
		running: map[string]*supervised{}, health: map[string]*PluginHealth{},
	}
}

// Start begins supervising spec under ctx (the daemon's lifetime); a plugin
// already supervised under the same name is stopped first, so an enable
// always runs the freshest spec (and token).
func (s *Supervisor) Start(ctx context.Context, spec Spec) error {
	if spec.Manifest == nil {
		return fmt.Errorf("supervise: spec has no manifest")
	}
	if err := ctx.Err(); err != nil {
		return fmt.Errorf("supervise %s: %w", spec.Manifest.Name, err)
	}
	name := spec.Manifest.Name
	s.Stop(name)
	runCtx, cancel := context.WithCancel(ctx)
	sv := &supervised{cancel: cancel, done: make(chan struct{})}
	s.mu.Lock()
	s.running[name] = sv
	s.health[name] = &PluginHealth{Name: name}
	s.mu.Unlock()
	s.wg.Add(1)
	go func() {
		defer s.wg.Done()
		defer close(sv.done)
		defer cancel()
		s.run(runCtx, spec)
		if c, ok := spec.LogSink.(io.Closer); ok && c != nil {
			if err := c.Close(); err != nil {
				s.logFor("plugin."+name).Warn("close plugin log sink", "error", err)
			}
		}
	}()
	return nil
}

// Stop ends supervision of one plugin (a disable): the child gets SIGTERM,
// then SIGKILL, and the health row disappears. A name not supervised is a
// no-op.
func (s *Supervisor) Stop(name string) {
	s.mu.Lock()
	sv := s.running[name]
	delete(s.running, name)
	delete(s.health, name)
	s.mu.Unlock()
	if sv == nil {
		return
	}
	sv.cancel()
	<-sv.done
}

// Wait blocks until every supervision goroutine has finished; the daemon
// calls it after the shared context is cancelled so no child outlives it.
func (s *Supervisor) Wait() { s.wg.Wait() }

// Health reports every supervised plugin's state, sorted by name.
func (s *Supervisor) Health() []PluginHealth {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]PluginHealth, 0, len(s.health))
	for _, h := range s.health {
		out = append(out, *h)
	}
	// Stable order for tables; the map is small.
	for i := 1; i < len(out); i++ {
		for j := i; j > 0 && out[j].Name < out[j-1].Name; j-- {
			out[j], out[j-1] = out[j-1], out[j]
		}
	}
	return out
}

// run is one plugin's supervision loop: start, wait, and restart per the
// manifest's policy until ctx ends or the policy says stop.
func (s *Supervisor) run(ctx context.Context, spec Spec) {
	m := spec.Manifest
	log := s.logFor("plugin." + m.Name)
	delay := s.backoff.Initial
	for attempt := 0; ; attempt++ {
		started := time.Now()
		exitErr := s.runOnce(ctx, spec, log)
		uptime := time.Since(started)
		exit := "exit ok"
		if exitErr != nil {
			exit = exitErr.Error()
		}
		s.mu.Lock()
		if h := s.health[m.Name]; h != nil {
			h.Running, h.PID, h.LastExit = false, 0, exit
		}
		s.mu.Unlock()
		if ctx.Err() != nil {
			log.Info("plugin stopped", "uptime_us", uptime.Microseconds())
			return
		}
		switch m.Restart {
		case "never":
			log.Warn("plugin exited; restart=never leaves it down", "exit", exit)
			return
		case "on-failure":
			if exitErr == nil {
				log.Info("plugin exited cleanly; restart=on-failure leaves it down")
				return
			}
		}
		delay = s.backoff.next(delay, uptime)
		if uptime >= s.backoff.ResetAfter {
			delay = s.backoff.Initial
		}
		log.Warn("plugin exited; restarting", "exit", exit, "backoff_us", delay.Microseconds(), "restarts", attempt+1)
		select {
		case <-ctx.Done():
			return
		case <-time.After(delay):
		}
		s.mu.Lock()
		if h := s.health[m.Name]; h != nil {
			h.Restarts++
		}
		s.mu.Unlock()
	}
}

// runOnce starts the child and waits for it to exit; a ctx cancellation sends
// SIGTERM, then SIGKILL after the grace. The returned error is the exit
// status (nil for a clean exit).
func (s *Supervisor) runOnce(ctx context.Context, spec Spec, log *slog.Logger) error {
	m := spec.Manifest
	argv := append([]string(nil), m.Command...)
	argv[0] = resolveCommand(argv[0], m.Dir)
	cmd := exec.Command(argv[0], argv[1:]...)
	cmd.Dir = m.Dir
	cmd.Env = spec.Env
	// Output flows through one in-process pipe rather than StderrPipe so
	// cmd.Wait can never close the read end mid-line: the exec package copies
	// into logW and Wait waits for that copy, and finish closes logW so the
	// scanner sees a clean EOF.
	logR, logW := io.Pipe()
	cmd.Stderr = logW
	var toolStdin io.WriteCloser
	var toolStdout io.ReadCloser
	if spec.OnStdio != nil {
		// A tools plugin's stdout is the MCP channel; only stderr is logged.
		stdin, err := cmd.StdinPipe()
		if err != nil {
			return fmt.Errorf("stdin pipe: %w", err)
		}
		stdout, err := cmd.StdoutPipe()
		if err != nil {
			return fmt.Errorf("stdout pipe: %w", err)
		}
		toolStdin, toolStdout = stdin, stdout
	} else {
		cmd.Stdout = logW
	}
	var lines sync.WaitGroup
	lines.Add(1)
	go func() {
		defer lines.Done()
		s.captureLines(logR, spec.LogSink, log)
	}()
	finish := func(err error) error {
		if cerr := logW.Close(); cerr != nil {
			log.Debug("close plugin log pipe", "error", cerr)
		}
		lines.Wait()
		return err
	}
	if err := cmd.Start(); err != nil {
		return finish(fmt.Errorf("start: %w", err))
	}
	if spec.OnStdio != nil {
		spec.OnStdio(toolStdin, toolStdout)
	}
	s.mu.Lock()
	if h := s.health[m.Name]; h != nil {
		h.Running, h.PID, h.Since = true, cmd.Process.Pid, time.Now().UTC()
	}
	s.mu.Unlock()
	log.Info("plugin started", "pid", cmd.Process.Pid, "restart", m.Restart)

	waited := make(chan error, 1)
	go func() { waited <- cmd.Wait() }()
	select {
	case err := <-waited:
		return finish(err)
	case <-ctx.Done():
	}
	if err := cmd.Process.Signal(syscall.SIGTERM); err != nil {
		log.Debug("signal plugin", "error", err)
	}
	select {
	case err := <-waited:
		return finish(err)
	case <-time.After(s.killGrace):
	}
	if err := cmd.Process.Kill(); err != nil {
		log.Debug("kill plugin", "error", err)
	}
	return finish(<-waited)
}

// captureLines copies one output stream line-by-line into the log sink and
// mirrors it to slog at debug (component plugin.<name>).
func (s *Supervisor) captureLines(r io.Reader, sink io.Writer, log *slog.Logger) {
	sc := bufio.NewScanner(r)
	sc.Buffer(make([]byte, 64<<10), 1<<20)
	for sc.Scan() {
		line := sc.Text()
		if sink != nil {
			if _, err := fmt.Fprintln(sink, line); err != nil {
				log.Debug("write plugin log", "error", err)
				sink = nil // one failed sink is not worth a warning per line
			}
		}
		log.Debug("output", "line", line)
	}
	if err := sc.Err(); err != nil {
		log.Debug("read plugin output", "error", err)
	}
}

// resolveCommand applies the manifest rule: a relative path in Command's first
// element ("./forge-x", "bin/x") resolves against the plugin directory; a bare
// word ("python3") stays a PATH lookup; an absolute path passes through.
func resolveCommand(arg0, dir string) string {
	if filepath.IsAbs(arg0) || !strings.Contains(arg0, "/") {
		return arg0
	}
	return filepath.Join(dir, arg0)
}

// NewToken mints one plugin token: 32 random bytes, hex-encoded, alongside
// the sha256 hex the store keeps. The plain token never rests on disk — the
// daemon re-mints (and re-hashes) it every time it starts the plugin process,
// so a restart invalidates the old token and no enable response needs to
// carry a secret.
func NewToken() (token, sha256hex string, err error) {
	var b [32]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", "", fmt.Errorf("mint plugin token: %w", err)
	}
	token = hex.EncodeToString(b[:])
	return token, HashToken(token), nil
}

// HashToken is the one spelling of how a presented token maps to the stored
// token_hash column.
func HashToken(token string) string {
	sum := sha256.Sum256([]byte(token))
	return hex.EncodeToString(sum[:])
}
