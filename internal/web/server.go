package web

import (
	"bytes"
	"context"
	"crypto/subtle"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/engine"
	"forge/internal/core/logging"
	"forge/internal/core/model"
	"forge/internal/core/modes"
	"forge/internal/core/plugin"
	"forge/internal/core/prompts"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
	"forge/internal/tools"
)

// maxBodyBytes bounds every request body; the largest legitimate body is an
// event batch (protocol.MaxEventBatchBytes) with headroom for a routine prompt.
const maxBodyBytes = 1 << 20

// traceBodyBytes is how much of a JSON body the trace level records.
const traceBodyBytes = 4 << 10

// shutdownGrace is how long Serve waits for in-flight requests after ctx ends.
const shutdownGrace = 5 * time.Second

// Server is the daemon's HTTP surface: one mux served on two listeners (the
// Unix socket and loopback TCP) whose only difference is the auth rule of
// DESIGN.md §1.1, decided per connection by the transport stamped in ConnContext.
// Engine holds the control plane's decision and orchestration state —
// everything the daemon knows that is not HTTP (MODULARIZATION.md §5): the
// store, the scheduler policy, config, and the injected func seams. Stage 2
// of the migration: it lives inside package controlplane and Server embeds
// it, so every existing call site and all internal tests keep compiling;
// Stage 3 cuts it out to internal/core/engine. No file defining an Engine
// method may import net/http (the boundary recipe checks).
type Engine struct {
	store            *store.Store
	policy           engine.SchedulerPolicy
	log              *slog.Logger
	now              func() time.Time
	home             string
	requiredLevel    func(mode string) int
	resolveModel     func(alias string) (string, bool)
	modelInfo        func(alias string) (config.ModelInfo, bool)
	modelAliases     []string
	routing          config.RoutingConfig
	runnerCapacities map[string]int
	setLogLevels     func(spec string) error
	logLevels        func() string
	allowHosts       []string
	gitConfig        map[string]string
	maxStackDepth    int
	tools            *tools.Registry
	kbDir            string
	modes            *modes.Registry
	pluginHealth     func() []plugin.PluginHealth
	pluginStart      func(name, token string) error
	pluginStop       func(name string)
	pluginRoots      []string

	// registerRepo backs DESIGN §1.3's repositories-on-the-fly; nil disables.
	registerRepo func(ctx context.Context, nameOrPath string) (protocol.Repository, error)
	// addRepo/archiveRepo/restoreRepo back the Repos page (POST /api/v1/
	// repositories and its archive/restore); nil disables those routes.
	addRepo     func(ctx context.Context, path, url, name string) (protocol.Repository, error)
	archiveRepo func(ctx context.Context, name, path string) (originURL string, err error)
	restoreRepo func(ctx context.Context, name, originURL string) (protocol.Repository, error)
	// startApp/stopApp/rebuildApp/appStatus back the Repos page's app lifecycle
	// (Start/Stop/Rebuild), supervised by the daemon; nil disables them.
	startApp   func(ctx context.Context, name, repoPath string) (protocol.AppStatus, error)
	stopApp    func(ctx context.Context, name, repoPath string) (protocol.AppStatus, error)
	rebuildApp func(ctx context.Context, name, repoPath string) (protocol.AppStatus, error)
	appStatus  func(ctx context.Context, name, repoPath string) (protocol.AppStatus, error)
	// modelCall is the concierge's one LLM primitive (daemon-injected); the
	// session maps hold per-sender conversation context.
	modelCall         func(ctx context.Context, system, user, model string) (string, error)
	prompts           func() *prompts.Library
	promptsReload     func() error
	assistantMu       sync.Mutex
	assistantSessions map[string][]assistantTurn
	assistantLastSeen map[string]time.Time
	// attention + quietHours drive the fuzzy Human Queue sweep (attention.go):
	// a non-critical question past its time-of-day SLA is auto-decided by the
	// decider model so its Work resumes.
	attentionCfg config.AttentionConfig
	quietHours   config.QuietHoursConfig
	// supervisionCfg drives the supervisor adjudication seam (supervision.go):
	// child-initiated budget negotiation and the watchdog that reaps (or, in
	// shadow mode, would-reap) wedged or spinning attempts.
	supervisionCfg config.SupervisionConfig

	// evalFn runs a mode's golden eval for auto-eval (autoeval.go); the real
	// impl is s.runEval, replaced by a fake in tests. exe is the daemon binary,
	// from which evalRoot discovers the checkout's evals/ and fixtures.
	evalFn evalRunner
	exe    string
	// evalRootMu guards the lazily-computed, cached evalRoot discovery
	// (evalRootDir, evalRootOK, evalRootDone).
	evalRootMu   sync.Mutex
	evalRootDir  string
	evalRootOK   bool
	evalRootDone bool
	// autoEvalSem is the single-slot semaphore that keeps at most one eval
	// running process-wide; autoEvalWG lets RunSweeper wait for its launched
	// eval goroutines before returning (no fire-and-forget, STYLE.md §3).
	autoEvalSem chan struct{}
	autoEvalWG  sync.WaitGroup
	// inflightMu guards inflightEval: proposal ids whose eval is running, so a
	// slow eval is not launched twice across sweep ticks.
	inflightMu   sync.Mutex
	inflightEval map[string]bool

	// flowLocks serializes workflow-run advances per run (flow_engine.go) so
	// a kick and the tick never execute one run's scripts twice concurrently.
	// flowStartFails carries definitional materialization failures (instance
	// id → message) from one apply to the next evaluation; a restart just
	// rediscovers them.
	flowMu         sync.Mutex
	flowLocks      map[string]*sync.Mutex
	flowStartFails map[string]string
}

// Server is the HTTP surface over the Engine: transport, auth, drain state,
// the inflight counter, and the SSE plumbing — nothing else
// (MODULARIZATION.md §5). The Engine is embedded so handlers and internal
// tests reach its fields and methods unchanged; Stage 5 narrows this to a
// named field when the packages separate.
type Server struct {
	*Engine
	version           string
	token             string
	transportOverride string
	mux               *http.ServeMux
	// draining refuses new claims and operator writes once set; heartbeats,
	// events, and completions keep flowing so running attempts finish (§1.4).
	draining atomic.Bool
	// inflight counts requests inside handle; the drain's exec waits for it to
	// reach zero so no response is cut off mid-write. SSE streams are not
	// counted — the drain flag closes them instead (§1.4).
	inflight atomic.Int64
	// execRestart replaces this process with a new binary once the drain is
	// idle; nil in processes that cannot (tests, or a server without listeners).
	execRestart func(execPath string) error
	// closed is closed by Serve on shutdown so long-lived streams end with a
	// retry hint instead of holding Shutdown for the whole grace period.
	closed    chan struct{}
	closeOnce sync.Once
	// streamInterval is the SSE endpoints' store poll cadence.
	streamInterval time.Duration
}

// ServerOptions are the inputs the server cannot derive itself.
type ServerOptions struct {
	Store         *store.Store
	Policy        engine.SchedulerPolicy // engine.AdmitAll in M1
	Logger        *slog.Logger           // component "web.http"
	Clock         func() time.Time       // defaults to time.Now
	Version       string                 // reported by the handshake
	Token         string                 // the worker token; required on TCP for worker/tool routes
	RequiredLevel func(mode string) int  // verification level a mode requires; nil means 1 for every mode
	Home          string                 // for daemon.json's state field on drain; "" skips the file
	// ResolveModel turns an alias into an executor model id. Nil installs M1's
	// fixed table; M10's routing replaces it through this seam.
	ResolveModel func(alias string) (id string, ok bool)
	// SetLogLevels and LogLevels back GET|POST /api/v1/log-level and the levels
	// workers inherit; when nil, GET answers "" and POST 501.
	SetLogLevels func(spec string) error
	LogLevels    func() string
	AllowHosts   []string          // [sandbox] allow_hosts, handed to workers as claim policy
	GitConfig    map[string]string // GIT_CONFIG_* the worker applies to attempt worktrees
	// MaxStackDepth caps the stack_on chain a claim may sit on ([integration]
	// max_stack_depth, DESIGN.md §20); 0 means the default of 2.
	MaxStackDepth int
	// TransportOverride forces the transport ("unix" | "tcp") instead of reading
	// it from the connection; tests use it because httptest listens on TCP.
	TransportOverride string
	// Tools are the MCP tool implementations behind /api/v1/tools; nil installs
	// tools.Defaults(). KbDir is the directory forge_kb_new writes notes into.
	Tools *tools.Registry
	KbDir string
	// Modes is the mode registry (M4); nil means only the built-in "run"
	// behaviour (RequiredLevel, envelope schema) until cmd/forge wires it.
	Modes *modes.Registry
	// ExecRestart execs the given binary in place of this process with the
	// lock and listener descriptors inherited (DESIGN.md §1.4); nil disables
	// the drain body's exec form.
	ExecRestart func(execPath string) error
	// RegisterRepo resolves an unregistered --repo value (a name under
	// projects_root, or an absolute checkout path) into a repository entry,
	// appending it to worker.toml (DESIGN §1.3); nil disables on-the-fly
	// registration and unknown repositories 404.
	RegisterRepo func(ctx context.Context, nameOrPath string) (protocol.Repository, error)
	// AddRepo (clone-if-URL then register), ArchiveRepo (drop from worker.toml
	// and delete the checkout, returning its clone URL), and RestoreRepo
	// (re-clone from a saved URL) back the Repos page; nil disables each route.
	AddRepo     func(ctx context.Context, path, url, name string) (protocol.Repository, error)
	ArchiveRepo func(ctx context.Context, name, path string) (originURL string, err error)
	RestoreRepo func(ctx context.Context, name, originURL string) (protocol.Repository, error)
	// StartApp/StopApp/RebuildApp/AppStatus drive a repository's app process
	// (Repos page Start/Stop/Rebuild); nil disables the routes.
	StartApp   func(ctx context.Context, name, repoPath string) (protocol.AppStatus, error)
	StopApp    func(ctx context.Context, name, repoPath string) (protocol.AppStatus, error)
	RebuildApp func(ctx context.Context, name, repoPath string) (protocol.AppStatus, error)
	AppStatus  func(ctx context.Context, name, repoPath string) (protocol.AppStatus, error)
	// Prompts returns the current persona/fragment library (the daemon's
	// last-good load of the prompts directory); nil disables personas.
	Prompts func() *prompts.Library
	// PromptsReload re-loads the library immediately (the page's save path
	// calls it so an edit composes without waiting for the 30s tick); nil
	// leaves reloads to the daemon loop.
	PromptsReload func() error
	// ModelCall runs one cheap model completion for the concierge (system +
	// user prompt → text); nil disables the assistant endpoint. It is also the
	// attention sweep's decider primitive (attention.go).
	ModelCall func(ctx context.Context, system, user, model string) (string, error)
	// Attention tunes the fuzzy Human Queue sweep; QuietHours (from [budget])
	// splits active vs quiet burndown. Zero Attention disables auto-decision
	// (WaitActiveMinutes 0), so tests and a bare daemon never auto-answer.
	Attention  config.AttentionConfig
	QuietHours config.QuietHoursConfig
	// Supervision tunes the supervisor adjudication seam (supervision.go); a
	// disabled Supervision (Enabled=false) never runs the watchdog and makes
	// forge_request_budget always continue.
	Supervision config.SupervisionConfig
	// StreamInterval overrides the SSE store poll cadence; 0 means 1 s.
	// Tests shorten it.
	StreamInterval time.Duration
	// PluginHealth reports the supervisor's live plugin state for GET
	// /api/v1/plugins, doctor, and the System page; nil means no supervisor.
	PluginHealth func() []plugin.PluginHealth
	// PluginStart (re)starts an enabled plugin's process with the freshly
	// minted token; nil means enable only records state (the next daemon
	// start runs it).
	PluginStart func(name, token string) error
	// PluginStop stops a disabled plugin's process; nil is a no-op.
	PluginStop func(name string)
	// PluginRoots is the ordered discovery roots install resolves a plugin
	// name against (earlier root wins) — the built-in <home>/plugins plus the
	// configured plugin_dirs (DESIGN.md §17), so an out-of-tree plugin can be
	// installed and started. Empty falls back to just <home>/plugins.
	PluginRoots []string
	// config.ModelInfo resolves an alias to its M10 routing info (runner, class,
	// price); nil disables routing so a claim uses the routine's single model
	// (the M1 path). ModelAliases is the sorted alias set the router considers
	// when a routine gives a tier but no explicit models list. Routing is the
	// [routing] policy. cmd/forge wires all three from the loaded config.Config.
	ModelInfo    func(alias string) (config.ModelInfo, bool)
	ModelAliases []string
	Routing      config.RoutingConfig
	// RunnerCapacities is each runner's capacity (a second scheduler slot
	// dimension, DESIGN.md §21); a runner absent or with capacity ≤ 0 is
	// unbounded (bounded only by worker slots).
	RunnerCapacities map[string]int
	// Executable is the daemon binary path (os.Executable); auto-eval walks up
	// from it to the checkout's evals/ and testdata/fixtures/. Empty disables
	// auto-eval discovery (tests inject EvalFn instead).
	Executable string
	// EvalFn overrides the auto-eval runner (autoeval.go); nil installs the
	// real s.runEval. Tests inject a fake so the suite never runs eval.Run.
	EvalFn func(ctx context.Context, mode string) (score float64, ok bool, err error)
}

// NewServer wires the routes. It does not listen; Serve does.
func NewServer(o ServerOptions) (*Server, error) {
	if o.Store == nil {
		return nil, fmt.Errorf("server: store is required")
	}
	if o.Policy == nil {
		o.Policy = engine.AdmitAll{}
	}
	if o.Logger == nil {
		o.Logger = slog.New(slog.DiscardHandler)
	}
	if o.Clock == nil {
		o.Clock = time.Now
	}
	if o.RequiredLevel == nil {
		o.RequiredLevel = func(string) int { return 1 }
	}
	if o.ResolveModel == nil {
		// Prefer the config-backed model table when wired; fall back to M1's
		// fixed aliases so tests and a bare daemon still resolve models.
		if o.ModelInfo != nil {
			o.ResolveModel = func(alias string) (string, bool) {
				info, ok := o.ModelInfo(alias)
				return info.ID, ok
			}
		} else {
			o.ResolveModel = defaultResolveModel
		}
	}
	if o.TransportOverride != "" && o.TransportOverride != transportUnix && o.TransportOverride != transportTCP {
		return nil, fmt.Errorf("server: transport override %q: want unix or tcp", o.TransportOverride)
	}
	if o.Tools == nil {
		o.Tools = tools.Defaults()
	}
	if len(o.PluginRoots) == 0 && o.Home != "" {
		o.PluginRoots = []string{filepath.Join(o.Home, "plugins")}
	}
	eng := &Engine{
		store: o.Store, policy: o.Policy, log: o.Logger, now: o.Clock, home: o.Home,
		requiredLevel: o.RequiredLevel, resolveModel: o.ResolveModel, modelInfo: o.ModelInfo, modelAliases: o.ModelAliases, routing: o.Routing, runnerCapacities: o.RunnerCapacities, setLogLevels: o.SetLogLevels, logLevels: o.LogLevels,
		allowHosts: o.AllowHosts, gitConfig: o.GitConfig, maxStackDepth: o.MaxStackDepth,
		tools: o.Tools, kbDir: o.KbDir, modes: o.Modes,
		pluginHealth: o.PluginHealth, pluginStart: o.PluginStart, pluginStop: o.PluginStop, pluginRoots: o.PluginRoots,
		registerRepo: o.RegisterRepo, addRepo: o.AddRepo, archiveRepo: o.ArchiveRepo, restoreRepo: o.RestoreRepo,
		startApp: o.StartApp, stopApp: o.StopApp, rebuildApp: o.RebuildApp, appStatus: o.AppStatus,
		modelCall: o.ModelCall, prompts: o.Prompts, promptsReload: o.PromptsReload, assistantSessions: map[string][]assistantTurn{}, assistantLastSeen: map[string]time.Time{},
		attentionCfg: o.Attention, quietHours: o.QuietHours, supervisionCfg: o.Supervision,
		exe: o.Executable, autoEvalSem: make(chan struct{}, 1), inflightEval: map[string]bool{},
		flowLocks: map[string]*sync.Mutex{},
	}
	eng.evalFn = o.EvalFn
	if eng.evalFn == nil {
		eng.evalFn = eng.runEval
	}
	s := &Server{
		Engine:  eng,
		version: o.Version, token: o.Token, transportOverride: o.TransportOverride, mux: http.NewServeMux(),
		execRestart: o.ExecRestart,
		closed:      make(chan struct{}), streamInterval: o.StreamInterval,
	}
	if s.streamInterval <= 0 {
		s.streamInterval = time.Second
	}
	s.routes()
	return s, nil
}

// routes is the whole route table; ui.go and stream.go add theirs to s.mux.
func (s *Server) routes() {
	m := s.mux
	m.HandleFunc("GET /healthz", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		if _, err := io.WriteString(w, "ok"); err != nil {
			s.log.Warn("write healthz", "error", err)
		}
	})
	m.HandleFunc("GET /api/v1/handshake", s.handle(s.handshake))
	m.HandleFunc("GET /api/v1/log-level", s.handle(s.getLogLevel))
	m.HandleFunc("POST /api/v1/log-level", s.handle(s.setLogLevel))
	m.HandleFunc("POST /api/v1/daemon/drain", s.handle(s.drain))
	s.streamRoutes(m)
	s.doctorRoutes(m)
	s.healthRoutes(m)
	s.pluginRoutes(m) // includes GET /api/v1/journal (JSON and SSE modes)

	m.HandleFunc("POST /api/v1/worker/register", s.handle(s.register))
	m.HandleFunc("POST /api/v1/worker/claim", s.handle(s.claim))
	m.HandleFunc("GET /api/v1/worker/attempts/{id}", s.handle(s.workerAttempt))
	m.HandleFunc("POST /api/v1/attempts/{id}/heartbeat", s.handle(s.heartbeat))
	m.HandleFunc("POST /api/v1/attempts/{id}/events", s.handle(s.postEvents))
	m.HandleFunc("POST /api/v1/attempts/{id}/complete", s.handle(s.complete))
	m.HandleFunc("PATCH /api/v1/attempts/{id}/cleanup", s.handle(s.cleanup))

	m.HandleFunc("GET /api/v1/tools", s.handle(s.listTools))
	s.kbRoutes(m)
	s.usageRoutes(m)
	s.statsRoutes(m)
	s.timelineRoutes(m)
	s.verifyRoutes(m)
	m.HandleFunc("POST /api/v1/tools/{name}", s.handle(s.callTool))

	m.HandleFunc("GET /api/v1/personas", s.handle(s.listPersonas))
	m.HandleFunc("GET /api/v1/personas/{name}", s.handle(s.getPersona))
	m.HandleFunc("GET /api/v1/prompts", s.handle(s.listPersonas))
	m.HandleFunc("GET /api/v1/prompts/{name...}", s.handle(s.getPromptFragment))
	m.HandleFunc("PUT /api/v1/prompts/{name...}", s.handle(s.putPromptFragment))
	m.HandleFunc("POST /api/v1/prompt-test", s.handle(s.promptTest))
	m.HandleFunc("GET /api/v1/prompt-tests", s.handle(s.listPromptTests))
	m.HandleFunc("GET /api/v1/routines", s.handle(s.listRoutines))
	m.HandleFunc("GET /api/v1/routine-templates", s.handle(s.listRoutineTemplates))
	m.HandleFunc("POST /api/v1/routines", s.handle(s.createRoutine))
	m.HandleFunc("GET /api/v1/routines/{name}", s.handle(s.getRoutine))
	m.HandleFunc("PUT /api/v1/routines/{name}", s.handle(s.updateRoutine))
	m.HandleFunc("DELETE /api/v1/routines/{name}", s.handle(s.archiveRoutine))
	m.HandleFunc("POST /api/v1/routines/{name}/run", s.handle(s.runRoutine))
	m.HandleFunc("GET /api/v1/routines/{name}/preview", s.handle(s.previewRoutine))
	m.HandleFunc("GET /api/v1/workflows", s.handle(s.listWorkflows))
	m.HandleFunc("POST /api/v1/workflows", s.handle(s.createWorkflow))
	m.HandleFunc("POST /api/v1/workflows/draft", s.handle(s.draftWorkflow))
	m.HandleFunc("GET /api/v1/workflows/{name}", s.handle(s.getWorkflow))
	m.HandleFunc("PUT /api/v1/workflows/{name}", s.handle(s.updateWorkflow))
	m.HandleFunc("PATCH /api/v1/workflows/{name}/layout", s.handle(s.updateWorkflowLayout))
	m.HandleFunc("DELETE /api/v1/workflows/{name}", s.handle(s.archiveWorkflow))
	m.HandleFunc("POST /api/v1/workflows/{name}/run", s.handle(s.runWorkflow))
	m.HandleFunc("GET /api/v1/workflows/{name}/runs", s.handle(s.workflowRuns))
	m.HandleFunc("GET /api/v1/workflow-runs/{id}", s.handle(s.getWorkflowRun))
	m.HandleFunc("POST /api/v1/workflow-runs/{id}/cancel", s.handle(s.cancelWorkflowRun))
	m.HandleFunc("POST /api/v1/workflow-runs/{id}/retry", s.handle(s.retryWorkflowRun))
	for _, base := range []string{"/api/v1/work", "/api/v1/tasks"} {
		m.HandleFunc("GET "+base, s.handle(s.listWork))
		m.HandleFunc("POST "+base, s.handle(s.createWork))
		m.HandleFunc("GET "+base+"/{id}", s.handle(s.getWork))
		m.HandleFunc("GET "+base+"/{id}/lineage", s.handle(s.workLineage))
		m.HandleFunc("DELETE "+base+"/{id}", s.handle(s.cancelWork))
		m.HandleFunc("PATCH "+base+"/{id}", s.handle(s.patchWork))
	}
	s.proposalRoutes(m)
	m.HandleFunc("GET /api/v1/queue", s.handle(s.queue))
	m.HandleFunc("POST /api/v1/questions/{id}/answer", s.handle(s.answer))
	m.HandleFunc("POST /api/v1/rpc/{method}", s.handle(s.rpc))
	m.HandleFunc("POST /api/v1/assistant/message", s.handle(s.assistantMessage))
	m.HandleFunc("GET /api/v1/attempts/{id}", s.handle(s.getAttempt))
	m.HandleFunc("GET /api/v1/attempts/{id}/events", s.handle(s.getEvents))
	m.HandleFunc("GET /api/v1/workers", s.handle(s.workers))
	m.HandleFunc("GET /api/v1/repositories", s.handle(s.repositories))
	s.repoRoutes(m)
	m.HandleFunc("GET /api/v1/attention", s.handle(s.attention))
}

// handlerFunc is the shape of every JSON route: validate, call the store,
// return what to encode. A nil body answers with the bare status (204).
type handlerFunc func(r *http.Request) (status int, body any, err error)

// handle adapts a handlerFunc to net/http so the error-to-status mapping has
// one home (fail) and no handler touches the ResponseWriter.
func (s *Server) handle(fn handlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		s.inflight.Add(1)
		defer s.inflight.Add(-1)
		status, body, err := fn(r)
		switch {
		case err != nil:
			s.fail(r.Context(), w, err)
		case body == nil:
			w.WriteHeader(status)
		default:
			writeJSON(r.Context(), s.log, w, status, body)
		}
	}
}

// Handler is the mux wrapped in the middleware chain: recover, request id and
// logging, transport auth, body limit.
func (s *Server) Handler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		id := logging.NewRequestID()
		ctx := logging.ContextWith(r.Context(), slog.String("request_id", id))
		r = r.WithContext(ctx)
		w.Header().Set("X-Request-ID", id)
		sw := &statusWriter{ResponseWriter: w, status: http.StatusOK}
		start := time.Now()
		transport := s.transport(ctx)
		defer func() {
			if p := recover(); p != nil {
				s.log.ErrorContext(ctx, "handler panic", "panic", fmt.Sprint(p), "method", r.Method, "path", r.URL.Path)
				if !sw.wrote {
					writeJSON(ctx, s.log, sw, http.StatusInternalServerError, protocol.Error{Error: "internal error"})
				}
			}
			s.log.DebugContext(ctx, "request", "method", r.Method, "path", r.URL.Path, "status", sw.status, "duration_us", time.Since(start).Microseconds(), "remote", r.RemoteAddr, "transport", transport)
		}()
		// A plugin bearer token is scope-checked on every transport — plugins
		// connect over the unix socket (DESIGN.md §17). Requests without a
		// bearer, with the worker token, or with an unknown token keep the
		// rules below exactly.
		if p := s.pluginForRequest(r); p != nil {
			if !pluginAuthorized(p, r.Method, r.URL.Path) {
				s.journalPluginDenied(ctx, p.Name, r.URL.Path)
				s.log.WarnContext(ctx, "plugin denied", "plugin", p.Name, "method", r.Method, "path", r.URL.Path)
				writeJSON(ctx, s.log, sw, http.StatusForbidden, protocol.Error{Error: "outside plugin scopes"})
				return
			}
			r = r.WithContext(context.WithValue(r.Context(), pluginCtxKey{}, p))
			ctx = r.Context()
		}
		if transport == transportTCP && requiresToken(r.Method, r.URL.Path) && !s.tokenOK(r) {
			// Tool routes also accept the attempt's own MCP token, which only
			// the handler can verify (the attempt id is in the query or body),
			// so they fall through to authorizeTools (handlers_tools.go).
			if !strings.HasPrefix(r.URL.Path, "/api/v1/tools") {
				writeJSON(ctx, s.log, sw, http.StatusUnauthorized, protocol.Error{Error: "token required"})
				return
			}
		}
		r.Body = http.MaxBytesReader(sw, r.Body, maxBodyBytes)
		if s.log.Enabled(ctx, logging.LevelTrace) && r.ContentLength != 0 && isJSON(r) {
			body, err := io.ReadAll(r.Body)
			if err != nil {
				s.fail(ctx, sw, badRequest("read body: %v", err))
				return
			}
			s.log.Log(ctx, logging.LevelTrace, "request body", "bytes", len(body), "body", string(body[:min(len(body), traceBodyBytes)]))
			r.Body = io.NopCloser(bytes.NewReader(body))
		}
		s.mux.ServeHTTP(sw, r)
	})
}

// Transports, as stamped by Serve's ConnContext.
const (
	transportUnix = "unix"
	transportTCP  = "tcp"
)

type transportKey struct{}

// transport reports how the request arrived; an unstamped context (a handler
// served outside Serve) is treated as TCP, the stricter rule.
func (s *Server) transport(ctx context.Context) string {
	if s.transportOverride != "" {
		return s.transportOverride
	}
	if t, ok := ctx.Value(transportKey{}).(string); ok {
		return t
	}
	return transportTCP
}

// requiresToken lists the routes that are the worker's or a tool's, not the
// operator's, over TCP: GET on an attempt's events is the operator reading a
// timeline, so only the writing methods on attempt sub-routes are gated.
func requiresToken(method, path string) bool {
	if strings.HasPrefix(path, "/api/v1/worker/") || strings.HasPrefix(path, "/api/v1/tools/") {
		return true
	}
	rest, ok := strings.CutPrefix(path, "/api/v1/attempts/")
	if !ok || method == http.MethodGet {
		return false
	}
	_, action, ok := strings.Cut(rest, "/")
	if !ok {
		return false
	}
	switch action {
	case "heartbeat", "events", "complete", "cleanup":
		return true
	}
	return false
}

func (s *Server) tokenOK(r *http.Request) bool {
	if s.token == "" {
		return false
	}
	presented, ok := strings.CutPrefix(r.Header.Get("Authorization"), "Bearer ")
	return ok && subtle.ConstantTimeCompare([]byte(presented), []byte(s.token)) == 1
}

func isJSON(r *http.Request) bool {
	ct := r.Header.Get("Content-Type")
	return ct == "" || strings.HasPrefix(ct, "application/json")
}

// statusWriter records what the handler answered, for the request log.
type statusWriter struct {
	http.ResponseWriter
	status int
	wrote  bool
}

func (w *statusWriter) WriteHeader(code int) {
	if !w.wrote {
		w.status, w.wrote = code, true
	}
	w.ResponseWriter.WriteHeader(code)
}

func (w *statusWriter) Write(b []byte) (int, error) {
	w.wrote = true
	return w.ResponseWriter.Write(b)
}

// Unwrap lets http.NewResponseController reach the real writer, so the SSE
// handlers can flush through the middleware.
func (w *statusWriter) Unwrap() http.ResponseWriter { return w.ResponseWriter }

// Serve runs the handler on both listeners until ctx is done, then shuts both
// down with shutdownGrace for in-flight requests. It returns nil on ctx
// cancellation and the first listener error otherwise. Either listener may be
// nil (a socket-only daemon), not both.
func (s *Server) Serve(ctx context.Context, unix, tcp net.Listener) error {
	if unix == nil && tcp == nil {
		return fmt.Errorf("serve: no listener")
	}
	h := s.Handler()
	var servers []*http.Server
	errs := make(chan error, 2)
	var wg sync.WaitGroup
	for _, l := range []struct {
		transport string
		listener  net.Listener
	}{{transportUnix, unix}, {transportTCP, tcp}} {
		if l.listener == nil {
			continue
		}
		transport := l.transport
		srv := &http.Server{
			Handler:           h,
			ReadHeaderTimeout: 10 * time.Second,
			ConnContext: func(ctx context.Context, _ net.Conn) context.Context {
				return context.WithValue(ctx, transportKey{}, transport)
			},
		}
		servers = append(servers, srv)
		wg.Add(1)
		go func(l net.Listener) {
			defer wg.Done()
			if err := srv.Serve(l); err != nil && !errors.Is(err, http.ErrServerClosed) {
				errs <- fmt.Errorf("serve %s listener: %w", transport, err)
			}
		}(l.listener)
	}
	var err error
	select {
	case <-ctx.Done():
	case err = <-errs:
	}
	s.closeStreams()
	shutdownCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), shutdownGrace)
	defer cancel()
	for _, srv := range servers {
		if serr := srv.Shutdown(shutdownCtx); serr != nil && err == nil {
			err = fmt.Errorf("shutdown: %w", serr)
		}
	}
	wg.Wait()
	return err
}

// closeStreams tells long-lived SSE handlers to end (with a retry hint) so
// Shutdown is not held open for the whole grace period by a healthy stream.
func (s *Server) closeStreams() { s.closeOnce.Do(func() { close(s.closed) }) }

// SetDraining flips the drain flag: claims and operator writes answer 503
// while it is on; the worker's heartbeat, events, complete, and cleanup keep
// working so running attempts finish.
func (s *Server) SetDraining(on bool) { s.draining.Store(on) }

// Draining reports the drain flag.
func (s *Server) Draining() bool { return s.draining.Load() }

// Model aliases M1 knows. M10 replaces the table through ServerOptions.ResolveModel.
func defaultResolveModel(alias string) (string, bool) {
	switch alias {
	case "haiku":
		return "claude-haiku-4-5-20251001", true
	case "sonnet":
		return "claude-sonnet-4-5", true
	case "opus":
		return "claude-opus-4-1", true
	}
	// A full model id ("claude-haiku-4-5-20251001") carries a "-<digit>"; it is
	// passed through unchanged.
	for i := 0; i+1 < len(alias); i++ {
		if alias[i] == '-' && alias[i+1] >= '0' && alias[i+1] <= '9' {
			return alias, true
		}
	}
	return "", false
}

// requestError is a client mistake: it becomes 400 with its message.
type requestError struct{ msg string }

func (e *requestError) Error() string { return e.msg }

func badRequest(format string, args ...any) error {
	return &requestError{msg: fmt.Sprintf(format, args...)}
}

// errDraining is answered 503 by the routes drain refuses.
var errDraining = errors.New("draining")

// fail maps an error to its status and body: store sentinels to 404/409, a
// requestError to 400, draining to 503, anything else to 500 logged once here.
func (s *Server) fail(ctx context.Context, w http.ResponseWriter, err error) {
	var re *requestError
	status := http.StatusInternalServerError
	switch {
	case errors.As(err, &re):
		status = http.StatusBadRequest
	case errors.Is(err, store.ErrNotFound):
		status = http.StatusNotFound
	case errors.Is(err, store.ErrConflict), errors.Is(err, store.ErrStaleGeneration), errors.Is(err, store.ErrLease), errors.Is(err, model.ErrTransition):
		status = http.StatusConflict
	case errors.Is(err, errDraining):
		status = http.StatusServiceUnavailable
	}
	if status == http.StatusInternalServerError {
		s.log.ErrorContext(ctx, "request failed", "error", err)
	}
	writeJSON(ctx, s.log, w, status, protocol.Error{Error: err.Error()})
}

// writeJSON encodes v with status. An encode failure after the header is out
// cannot be reported to the client, so it is logged.
func writeJSON(ctx context.Context, log *slog.Logger, w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	if err := json.NewEncoder(w).Encode(v); err != nil {
		log.WarnContext(ctx, "write response", "error", err)
	}
}

// decodeJSON reads one JSON body into v; a body over maxBodyBytes and malformed
// JSON are both the client's fault.
func decodeJSON(r *http.Request, v any) error {
	if err := json.NewDecoder(r.Body).Decode(v); err != nil {
		var mbe *http.MaxBytesError
		if errors.As(err, &mbe) {
			return badRequest("body exceeds %d bytes", maxBodyBytes)
		}
		return badRequest("decode body: %v", err)
	}
	return nil
}

// pathID reads a {id} segment and validates it as a Forge ID before it reaches
// a query or a log line.
func pathID(r *http.Request) (string, error) {
	id := r.PathValue("id")
	if err := model.ValidateID(id); err != nil {
		return "", badRequest("%v", err)
	}
	return id, nil
}

// listLimit reads ?limit= with STYLE.md §7's bounds: default 50, max 500.
func listLimit(r *http.Request) (int, error) {
	raw := r.URL.Query().Get("limit")
	if raw == "" {
		return 50, nil
	}
	n, err := strconv.Atoi(raw)
	if err != nil || n <= 0 {
		return 0, badRequest("limit %q: want a positive integer", raw)
	}
	return min(n, 500), nil
}

func (s *Server) handshake(*http.Request) (int, any, error) {
	state := "running"
	if s.Draining() {
		state = "draining"
	}
	return http.StatusOK, protocol.Handshake{Version: s.version, SchemaVersion: s.store.SchemaVersion(), State: state, PID: os.Getpid()}, nil
}

// logLevelBody is GET|POST /api/v1/log-level's shape in both directions.
type logLevelBody struct {
	Levels string `json:"levels"`
}

func (s *Server) getLogLevel(*http.Request) (int, any, error) {
	return http.StatusOK, logLevelBody{Levels: s.currentLogLevels()}, nil
}

func (s *Server) currentLogLevels() string {
	if s.logLevels == nil {
		return ""
	}
	return s.logLevels()
}

func (s *Server) setLogLevel(r *http.Request) (int, any, error) {
	if s.setLogLevels == nil {
		return http.StatusNotImplemented, protocol.Error{Error: "log levels are not adjustable in this process"}, nil
	}
	var body logLevelBody
	if err := decodeJSON(r, &body); err != nil {
		return 0, nil, err
	}
	if err := s.setLogLevels(body.Levels); err != nil {
		return 0, nil, badRequest("%v", err)
	}
	s.log.InfoContext(r.Context(), "log levels changed", "levels", body.Levels)
	return http.StatusOK, logLevelBody{Levels: s.currentLogLevels()}, nil
}

func (s *Server) journal(r *http.Request) (int, any, error) {
	var since int64
	if raw := r.URL.Query().Get("since"); raw != "" {
		n, err := strconv.ParseInt(raw, 10, 64)
		if err != nil || n < 0 {
			return 0, nil, badRequest("since %q: want a journal id", raw)
		}
		since = n
	}
	limit, err := listLimit(r)
	if err != nil {
		return 0, nil, err
	}
	entries, err := s.store.JournalSince(r.Context(), since, limit)
	if err != nil {
		return 0, nil, err
	}
	if entries == nil {
		entries = []store.JournalEntry{}
	}
	return http.StatusOK, entries, nil
}

// MountRoot mounts the server-rendered UI (or any root handler) at "/". The
// ui package imports web for its shared read-path surface, so the server
// takes a plain http.Handler here rather than importing ui back.
func (s *Server) MountRoot(h http.Handler) { s.mux.Handle("/", h) }
