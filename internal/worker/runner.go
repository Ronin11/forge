package worker

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	mrand "math/rand/v2"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"sync"
	"syscall"
	"time"

	"golang.org/x/sync/errgroup"

	"forge/internal/core/model"
	"forge/internal/logging"
	"forge/internal/protocol"
)

// Loop cadences (DESIGN.md §7.2).
const (
	registerInterval  = 30 * time.Second
	pollInterval      = 2 * time.Second
	reconcileInterval = 5 * time.Minute
	shutdownTimeout   = 30 * time.Second
)

// Worker is the long-lived process: validated repositories, a slot pool, the
// claim/registration/reconcile loops, and one Runner for attempts.
type Worker struct {
	cfg      *Config
	id       string
	version  string
	client   daemon
	runner   *Runner
	log      *slog.Logger
	handler  *logging.Handler
	clock    func() time.Time
	lockFile *os.File
	// capsMu guards caps: New seeds it, the runner probe loop rewrites the
	// runner:<name> entries every ~2 min, and registration reads a snapshot.
	capsMu   sync.Mutex
	caps     map[string]string
	slots    chan struct{}
	retained *retainedSet
	forgeBin string
	rng      *mrand.Rand
	mu       sync.Mutex // guards active
	active   map[string]context.CancelFunc
}

// retainedSet is what registration advertises as retained worktrees.
type retainedSet struct {
	mu    sync.Mutex
	items map[string]protocol.RetainedWorktree
}

func (r *retainedSet) set(rw protocol.RetainedWorktree) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.items[rw.AttemptID] = rw
}

func (r *retainedSet) remove(attemptID string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	delete(r.items, attemptID)
}

func (r *retainedSet) list() []protocol.RetainedWorktree {
	r.mu.Lock()
	defer r.mu.Unlock()
	out := make([]protocol.RetainedWorktree, 0, len(r.items))
	for _, v := range r.items {
		out = append(out, v)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].AttemptID < out[j].AttemptID })
	return out
}

// WorkerOptions are the inputs New cannot derive from the config.
type WorkerOptions struct {
	Config   *Config
	Version  string
	Handler  *logging.Handler
	Clock    func() time.Time
	ForgeBin string // absolute path of this binary, for the MCP config
	Daemon   daemon // nil → a Client for Config.Daemon
}

// New validates the configuration against the machine: the data directory
// lock, every repository, every executor command, and the capabilities it can
// advertise. It does not talk to the daemon. Daemon, when nil, is a Client for
// the configured address; tests pass a fake.
func New(ctx context.Context, o WorkerOptions) (w *Worker, err error) {
	if o.Clock == nil {
		o.Clock = time.Now
	}
	if o.Handler == nil {
		o.Handler = logging.Discard()
	}
	cfg := o.Config
	log := o.Handler.For("worker")
	if err := os.MkdirAll(cfg.DataDir, 0o700); err != nil {
		return nil, fmt.Errorf("create data dir: %w", err)
	}
	lock, err := lockDataDir(cfg.DataDir)
	if err != nil {
		return nil, err
	}
	defer func() {
		if err != nil {
			err = errors.Join(err, lock.Close())
		}
	}()
	id, err := workerIdentity(cfg.DataDir)
	if err != nil {
		return nil, err
	}
	git := Git{}
	repos := map[string]*Repository{}
	for _, name := range cfg.RepositoryNames() {
		if name == greenfieldRepoName {
			// The name is reserved for the virtual repository ([greenfield]
			// projects_root); a checkout under it would shadow that contract.
			return nil, fmt.Errorf("repository name %q is reserved; configure [greenfield] projects_root instead", name)
		}
		rc := cfg.Repositories[name]
		r, err := git.ValidateRepository(ctx, name, rc.Path, rc.BaseBranch)
		if err != nil {
			return nil, err
		}
		for other, existing := range repos {
			if existing.Path == r.Path {
				return nil, fmt.Errorf("repositories %s and %s resolve to the same path", name, other)
			}
		}
		r.Project = rc.Project
		repos[name] = r
		log.InfoContext(ctx, "repository validated", "name", name, "path", r.Path, "origin", r.OriginIdentity)
	}
	if cfg.Greenfield.ProjectsRoot != "" {
		// The virtual greenfield repository: not a checkout, never validated as
		// one; attempts on it git-init their own directory (MODES.md §greenfield).
		repos[greenfieldRepoName] = &Repository{Name: greenfieldRepoName, Path: cfg.Greenfield.ProjectsRoot, OriginIdentity: greenfieldOriginIdentity, Project: "default"}
	}
	executors, err := ExecutorsFromConfig(cfg.Executors)
	if err != nil {
		return nil, err
	}
	caps := map[string]string{}
	for name, ec := range cfg.Executors {
		state := "ready"
		if _, err := exec.LookPath(ec.Command[0]); err != nil {
			state = "missing"
		}
		caps["executor:"+name] = state
	}
	// NewSandbox and the capability agree by construction: nil is `missing`.
	sandbox := NewSandbox(filepath.Dir(cfg.DataDir), cfg.Sandbox)
	if sandbox != nil {
		caps["sandbox"] = "ready"
	} else {
		caps["sandbox"] = "missing"
	}
	if _, err := os.Stat(filepath.Join(filepath.Dir(cfg.DataDir), "deps", "node_modules", "playwright")); err == nil {
		caps["browser"] = "ready"
	} else {
		caps["browser"] = "missing"
	}
	token := ""
	if cfg.TokenFile != "" {
		if b, err := os.ReadFile(cfg.TokenFile); err == nil {
			token = string(trimSpace(b))
		}
	}
	var client daemon = o.Daemon
	if client == nil {
		c, err := NewClient(cfg.Daemon, token, 10*time.Second)
		if err != nil {
			return nil, err
		}
		client = c
	}
	manifests, err := NewManifestStore(cfg.DataDir, id, o.Clock)
	if err != nil {
		return nil, err
	}
	w = &Worker{
		cfg: cfg, id: id, version: o.Version, client: client, log: log, handler: o.Handler, clock: o.Clock,
		lockFile: lock, caps: caps, slots: make(chan struct{}, cfg.MaxConcurrent), retained: &retainedSet{items: map[string]protocol.RetainedWorktree{}},
		forgeBin: o.ForgeBin, rng: mrand.New(mrand.NewPCG(uint64(o.Clock().UnixNano()), 7)), active: map[string]context.CancelFunc{},
	}
	w.runner = &Runner{cfg: cfg, workerID: id, git: git, executors: executors, parsers: DefaultParsers(), manifests: manifests, daemon: client, repos: repos, log: o.Handler.For("worker.attempt"), clock: o.Clock, forgeBin: o.ForgeBin, sandbox: sandbox}
	return w, nil
}

// ID is the stable worker identity.
func (w *Worker) ID() string { return w.id }

// Capabilities are what registration advertises (a snapshot; the runner probe
// loop mutates the live map under capsMu).
func (w *Worker) Capabilities() map[string]string {
	w.capsMu.Lock()
	defer w.capsMu.Unlock()
	out := make(map[string]string, len(w.caps))
	for k, v := range w.caps {
		out[k] = v
	}
	return out
}

// Close releases the data directory lock.
func (w *Worker) Close() error { return w.lockFile.Close() }

// lockDataDir takes the flock that makes two workers on one data dir impossible.
func lockDataDir(dataDir string) (*os.File, error) {
	path := filepath.Join(dataDir, "lock")
	f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, fmt.Errorf("open %s: %w", path, err)
	}
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		cerr := f.Close()
		return nil, errors.Join(fmt.Errorf("another forge worker owns %s: %w", dataDir, err), cerr)
	}
	return f, nil
}

// workerIdentity reads or creates <dataDir>/worker-id: the id is identity, not
// a credential, and survives restarts so retained worktrees stay attributable.
func workerIdentity(dataDir string) (string, error) {
	path := filepath.Join(dataDir, "worker-id")
	b, err := os.ReadFile(path)
	if err == nil {
		id := string(trimSpace(b))
		if err := model.ValidateID(id); err != nil {
			return "", fmt.Errorf("%s: %w", path, err)
		}
		return id, nil
	}
	if !os.IsNotExist(err) {
		return "", fmt.Errorf("read %s: %w", path, err)
	}
	id := model.NewID()
	if err := os.WriteFile(path, []byte(id+"\n"), 0o600); err != nil {
		return "", fmt.Errorf("write %s: %w", path, err)
	}
	return id, nil
}

func trimSpace(b []byte) []byte {
	for len(b) > 0 && (b[len(b)-1] == '\n' || b[len(b)-1] == '\r' || b[len(b)-1] == ' ') {
		b = b[:len(b)-1]
	}
	return b
}

// Run registers, reconciles, and loops until ctx is done, then stops active
// attempts and waits for them (bounded).
func (w *Worker) Run(ctx context.Context) error {
	w.probeRunners(ctx)
	w.log.InfoContext(ctx, "worker starting", "id", w.id, "name", w.cfg.Name, "slots", w.cfg.MaxConcurrent, "capabilities", w.Capabilities())
	if err := w.reconcile(ctx); err != nil {
		w.log.WarnContext(ctx, "reconcile on start", "error", err)
	}
	w.register(ctx)
	g, gctx := errgroup.WithContext(ctx)
	g.Go(func() error { w.registerLoop(gctx); return nil })
	g.Go(func() error { w.reconcileLoop(gctx); return nil })
	g.Go(func() error { w.pollLoop(gctx); return nil })
	g.Go(func() error { w.runnerProbeLoop(gctx); return nil })
	if w.handler != nil {
		g.Go(func() error {
			logging.WatchSIGUSR1(w.handler, w.log, nil).Run(gctx)
			return nil
		})
	}
	<-ctx.Done()
	w.log.InfoContext(ctx, "worker stopping; cancelling active attempts")
	w.mu.Lock()
	for _, cancel := range w.active {
		cancel()
	}
	w.mu.Unlock()
	if err := g.Wait(); err != nil {
		return err
	}
	deadline := time.Now().Add(shutdownTimeout)
	for w.activeCount() > 0 && time.Now().Before(deadline) {
		time.Sleep(50 * time.Millisecond)
	}
	if n := w.activeCount(); n > 0 {
		w.log.WarnContext(ctx, "attempts still active at shutdown; reconcile will finish them", "count", n)
	}
	return nil
}

func (w *Worker) activeCount() int {
	w.mu.Lock()
	defer w.mu.Unlock()
	return len(w.active)
}

func (w *Worker) registerRequest() protocol.RegisterRequest {
	list := w.runner.repoList()
	repos := make([]protocol.Repository, 0, len(list))
	for _, r := range list {
		repos = append(repos, protocol.Repository{Name: r.Name, Path: r.Path, OriginIdentity: r.OriginIdentity, BaseBranch: r.BaseBranch, Project: r.Project})
	}
	return protocol.RegisterRequest{
		WorkerID: w.id, Name: w.cfg.Name, Version: w.version, MaxConcurrent: w.cfg.MaxConcurrent, Active: len(w.slots),
		Executors: w.runner.executors.Names(), Capabilities: w.Capabilities(), Repositories: repos, Retained: w.retained.list(),
	}
}

// register advertises the worker and applies the daemon's log levels.
func (w *Worker) register(ctx context.Context) bool {
	resp, err := w.client.Register(ctx, w.registerRequest())
	if err != nil {
		w.log.DebugContext(ctx, "register failed", "error", err)
		return false
	}
	if resp.LogLevels != "" && w.handler != nil {
		if levels, err := logging.ParseLevels(resp.LogLevels, slog.LevelInfo); err == nil {
			w.handler.SetLevels(levels)
		}
	}
	return true
}

func (w *Worker) registerLoop(ctx context.Context) {
	t := time.NewTicker(registerInterval)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			w.refreshRepos(ctx)
			w.register(ctx)
		}
	}
}

// refreshRepos re-reads worker.toml and validates repositories added since
// start (DESIGN §1.3: the daemon appends on-the-fly registrations; the worker
// advertises them on its next registration tick, ≤30s). Removals and edits of
// existing entries still need a worker restart — only additions are live.
func (w *Worker) refreshRepos(ctx context.Context) {
	cfg, err := LoadConfig(w.cfg.Path())
	if err != nil {
		w.log.WarnContext(ctx, "refresh repositories: reload worker.toml", "error", err)
		return
	}
	for _, name := range cfg.RepositoryNames() {
		if name == greenfieldRepoName {
			continue
		}
		if _, ok := w.runner.repo(name); ok {
			continue
		}
		rc := cfg.Repositories[name]
		r, err := w.runner.git.ValidateRepository(ctx, name, rc.Path, rc.BaseBranch)
		if err != nil {
			w.log.WarnContext(ctx, "refresh repositories: validate", "name", name, "error", err)
			continue
		}
		r.Project = rc.Project
		if err := w.runner.addRepo(r); err != nil {
			w.log.WarnContext(ctx, "refresh repositories", "name", name, "error", err)
			continue
		}
		w.log.InfoContext(ctx, "repository added on the fly", "name", name, "path", r.Path, "origin", r.OriginIdentity)
	}
}

func (w *Worker) reconcileLoop(ctx context.Context) {
	t := time.NewTicker(reconcileInterval)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			if err := w.reconcile(ctx); err != nil {
				w.log.WarnContext(ctx, "reconcile", "error", err)
			}
		}
	}
}

// pollLoop claims while a slot is free: one request in flight, a successful
// claim immediately tries again, an empty answer waits a jittered interval, a
// daemon outage backs off.
func (w *Worker) pollLoop(ctx context.Context) {
	delay := time.Duration(0)
	backoff := heartbeatRetryMin
	for {
		select {
		case <-ctx.Done():
			return
		case <-time.After(delay):
		}
		select {
		case w.slots <- struct{}{}:
		case <-ctx.Done():
			return
		}
		claim, err := w.claimOnce(ctx)
		switch {
		case err != nil:
			<-w.slots
			w.log.DebugContext(ctx, "claim failed", "error", err, "retry_in", backoff)
			delay = backoff
			if backoff = min(backoff*2, heartbeatRetryMax); backoff == 0 {
				backoff = heartbeatRetryMin
			}
			w.register(ctx) // re-register on the first success after a failure
		case claim == nil:
			<-w.slots
			delay = pollInterval + time.Duration(w.rng.Int64N(int64(pollInterval/5)))
			backoff = heartbeatRetryMin
		default:
			backoff = heartbeatRetryMin
			delay = 0
			w.startAttempt(ctx, claim)
		}
	}
}

func (w *Worker) claimOnce(ctx context.Context) (*protocol.Claim, error) {
	lease, err := randomToken()
	if err != nil {
		return nil, err
	}
	claim, err := w.client.Claim(ctx, protocol.ClaimRequest{WorkerID: w.id, ClaimRequestID: model.NewID(), LeaseToken: lease})
	if err != nil || claim == nil {
		return claim, err
	}
	claim.LeaseToken = lease
	return claim, nil
}

func randomToken() (string, error) {
	var b [24]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", fmt.Errorf("mint lease token: %w", err)
	}
	return hex.EncodeToString(b[:]), nil
}

// startAttempt runs the claim in its own goroutine holding the slot.
func (w *Worker) startAttempt(ctx context.Context, claim *protocol.Claim) {
	actx, cancel := context.WithCancel(context.WithoutCancel(ctx))
	w.mu.Lock()
	w.active[claim.AttemptID] = cancel
	w.mu.Unlock()
	w.log.InfoContext(ctx, "claimed", "attempt_id", claim.AttemptID, "routine", claim.RoutineName, "repository", claim.Repository, "resume", claim.Resume != nil)
	go func() {
		defer func() {
			<-w.slots
			w.mu.Lock()
			delete(w.active, claim.AttemptID)
			w.mu.Unlock()
			cancel()
		}()
		w.runner.Run(actx, claim)
		if m, err := w.runner.manifests.Load(claim.AttemptID); err == nil {
			w.noteRetention(m)
		}
	}()
}

// noteRetention keeps the registration's retained list in step with manifests.
func (w *Worker) noteRetention(m *Manifest) {
	switch m.Lifecycle {
	case ManifestRetained, ManifestInconsistent:
		w.retained.set(protocol.RetainedWorktree{AttemptID: m.AttemptID, Path: m.WorktreePath, Reason: m.RetentionReason, CleanupCommand: m.CleanupCommand})
	default:
		w.retained.remove(m.AttemptID)
	}
}
