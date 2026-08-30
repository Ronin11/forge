package worker

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"forge/internal/logging"
	"forge/internal/model"
	"forge/internal/protocol"
)

// attemptDaemon is the slice of the daemon an attempt uses, so tests can drive
// an attempt against an in-memory fake.
type attemptDaemon interface {
	eventSink
	Heartbeat(ctx context.Context, attemptID string, req protocol.HeartbeatRequest) (*protocol.HeartbeatResponse, error)
	Complete(ctx context.Context, attemptID string, req protocol.CompleteRequest) (*protocol.CompleteResponse, error)
}

// daemon is everything the worker loop and reconcile need; *Client implements it.
type daemon interface {
	attemptDaemon
	Register(ctx context.Context, req protocol.RegisterRequest) (*protocol.RegisterResponse, error)
	Claim(ctx context.Context, req protocol.ClaimRequest) (*protocol.Claim, error)
	PatchCleanup(ctx context.Context, attemptID string, p protocol.CleanupPatch) error
	Attempt(ctx context.Context, attemptID string) (*AttemptState, error)
}

// Timing constants of the attempt loop (DESIGN.md §5, §14).
const (
	heartbeatInterval = 10 * time.Second
	heartbeatRetryMin = time.Second
	heartbeatRetryMax = 10 * time.Second
	leaseLostAfter    = 120 * time.Second
)

// Runner executes one claim end to end. It is built once per worker and is
// safe for concurrent attempts.
type Runner struct {
	cfg       *Config
	workerID  string
	git       Git
	executors Executors
	parsers   Parsers
	manifests *ManifestStore
	daemon    attemptDaemon
	repos     map[string]*Repository
	log       *slog.Logger
	clock     func() time.Time
	forgeBin  string
	// repoLocks serialise git metadata operations per checkout.
	repoLocks sync.Map // name → *sync.Mutex
}

// attempt is the state of one run of the Runner.
type attempt struct {
	r        *Runner
	claim    *protocol.Claim
	repo     *Repository
	ctx      context.Context // carries attempt_id/target_id/work_id for logs
	log      *slog.Logger
	emitter  *Emitter
	manifest *Manifest
	ft       *ForgeToml

	// cancelled is set by the heartbeat goroutine; stopReason is what it wants
	// the process stopped for.
	mu         sync.Mutex
	process    *Process
	stopReason string
}

// Run executes the claim and reports to the daemon. It returns only when the
// attempt is fully reported (or definitively cannot be), never earlier.
func (r *Runner) Run(ctx context.Context, claim *protocol.Claim) {
	ctx = logging.ContextWith(ctx, slog.String("attempt_id", claim.AttemptID), slog.String("target_id", claim.TargetID), slog.String("work_id", claim.WorkID))
	a := &attempt{r: r, claim: claim, ctx: ctx, log: r.log}
	start := r.clock()
	nextSeq, elapsedBefore, launches := 0, int64(0), 0
	if claim.Resume != nil {
		if m, err := r.manifests.Load(claim.AttemptID); err == nil {
			nextSeq, elapsedBefore, launches, a.manifest = m.NextSeq, m.ElapsedBeforeUS, m.Launches, m
		} else {
			a.log.WarnContext(ctx, "resume without a manifest; starting a fresh timeline", "error", err)
		}
	}
	a.emitter = NewEmitter(claim.AttemptID, r.daemon, r.log, r.clock, nextSeq, elapsedBefore)
	emitCtx, stopEmit := context.WithCancel(ctx)
	var emitWG sync.WaitGroup
	emitWG.Add(1)
	go func() { defer emitWG.Done(); a.emitter.Run(emitCtx) }()

	hbCtx, stopHB := context.WithCancel(ctx)
	var hbWG sync.WaitGroup
	hbWG.Add(1)
	go func() { defer hbWG.Done(); a.heartbeatLoop(hbCtx) }()

	req := a.execute(ctx, start, launches)

	stopHB()
	hbWG.Wait()
	a.emitter.Flush(ctx)
	stopEmit()
	emitWG.Wait()
	// One last flush with a fresh, bounded context: the daemon may have come
	// back since the loop's context ended.
	flushCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), 10*time.Second)
	defer cancel()
	a.emitter.Flush(flushCtx)
	a.report(flushCtx, req)
}

// execute runs the phases and returns the completion report. Every failure
// path still produces a report; a worktree, once created, is always inspected
// and cleaned up or retained.
func (a *attempt) execute(ctx context.Context, start time.Time, launches int) protocol.CompleteRequest {
	c := a.claim
	req := protocol.CompleteRequest{LeaseToken: leaseTokenFor(c), StartedAt: start.UTC(), Launches: launches}
	a.emitter.Lifecycle("claimed", map[string]any{"routine": c.RoutineName, "repository": c.Repository, "mode": c.Mode, "resume": c.Resume != nil})

	// 1. Validate.
	if err := a.validate(); err != nil {
		a.emitter.Lifecycle("prepare failed", map[string]any{"error": shortError(err)})
		return a.fail(req, model.ReasonPrepareFailed, err)
	}

	// 2–4. Git phases under the repository lock; skipped on a resume.
	wt := filepath.Join(a.r.cfg.DataDir, "worktrees", c.AttemptID)
	branch := model.BranchName(c.RoutineName, c.AttemptID)
	if a.manifest == nil {
		if err := a.prepareWorktree(ctx, wt, branch); err != nil {
			a.emitter.Lifecycle("prepare failed", map[string]any{"error": shortError(err)})
			return a.finish(ctx, req, model.Failed, model.ReasonPrepareFailed, err, nil)
		}
	} else {
		a.emitter.Lifecycle("resuming in existing worktree", map[string]any{"worktree": wt, "launches": launches})
	}
	m := a.manifest
	if a.ft == nil {
		ft, err := ReadForgeToml(m.WorktreePath)
		if err != nil {
			a.log.WarnContext(ctx, "forge.toml unreadable; treating as absent", "error", err)
		}
		a.ft = ft
	}

	// 5. Manifest + MCP config + prompt.
	span := a.emitter.StartSpan("manifest", "manifest", "", nil)
	mcpConfig, err := a.writeMCPConfig()
	if err == nil {
		m.Lifecycle, m.DeadlineAt, m.Launches = ManifestWorktreeCreated, a.r.clock().Add(a.remaining(launches)), launches
		err = a.r.manifests.Write(m)
	}
	span.End(err, nil)
	if err != nil {
		return a.finish(ctx, req, model.Failed, model.ReasonPrepareFailed, err, nil)
	}
	var ftJSON json.RawMessage
	if a.ft != nil {
		if b, jerr := json.Marshal(a.ft); jerr == nil {
			ftJSON = b
		}
	}
	a.heartbeat(ctx, protocol.HeartbeatRequest{State: model.Preparing, Phase: "manifest", Worktree: m.WorktreePath, Branch: m.Branch, BaseBranch: m.BaseBranch, BaseCommit: m.BaseCommit, ForgeToml: ftJSON, PromptVersion: a.promptVersion()})

	// 6. Agent.
	result, exit, ferr := a.runAgent(ctx, launches+1, mcpConfig)
	req.Launches = launches + 1
	req.SessionID = result.SessionID
	req.ExitCode = exit.Code
	req.IsError = result.IsError
	req.NumTurns = result.NumTurns
	req.Usage = result.Usage
	req.CostUSD = result.CostUSD
	req.ResultText = result.Text
	req.Result = result.Structured
	req.OutputPath = a.outputPath()
	req.OutputBytes, req.OutputTruncated = exit.OutputBytes, exit.Truncated
	if ferr != nil {
		return a.finish(ctx, req, model.Failed, model.ReasonLaunchFailed, ferr, nil)
	}
	m.SessionID = result.SessionID

	// Outcome of the process.
	state, reason := model.Succeeded, model.FailureReason("")
	switch {
	case a.stopped() == "cancelled":
		state, reason = model.Cancelled, model.ReasonCancelled
	case a.stopped() == "lease_lost":
		state, reason = model.Failed, model.ReasonLeaseExpired
	case exit.TimedOut:
		state, reason = model.Failed, model.ReasonTimeout
	case exit.Code != 0 || result.IsError:
		state, reason = model.Failed, model.ReasonExitNonzero
	}
	var env *ResultEnvelope
	hasEnv := false
	if state == model.Succeeded {
		var perr error
		env, hasEnv, perr = ParseEnvelope(result.Structured)
		if perr != nil {
			state, reason = model.Failed, model.ReasonResultUnparseable
			a.emitter.Lifecycle("result unparseable", map[string]any{"error": shortError(perr)})
		}
	}
	if state == model.Succeeded && env != nil && env.NeedsInput != nil {
		if !c.Autonomy.AllowsQuestions() {
			state, reason = model.Failed, model.ReasonAmbiguityAtAuto
		} else {
			state = model.WaitingHuman
			req.Question = &protocol.QuestionRequest{Text: env.NeedsInput.Question, Options: env.NeedsInput.Options, Context: env.NeedsInput.Context, Checkpoint: env.NeedsInput.Checkpoint}
		}
	}
	return a.finish(ctx, req, state, reason, nil, func(git protocol.GitOutcome) protocol.Verification {
		if state != model.Succeeded {
			return protocol.Verification{}
		}
		var checks []CheckResult
		declared := a.ft != nil && len(a.ft.Checks) > 0
		if declared {
			checks = RunChecks(ctx, m.WorktreePath, a.ft, a.env())
		}
		return Verify(env, hasEnv, git, checks, declared)
	})
}

// finish runs git_inspect, verify, and cleanup for a worktree that exists, and
// fills the request's terminal fields. verify may be nil when nothing ran.
func (a *attempt) finish(ctx context.Context, req protocol.CompleteRequest, state model.State, reason model.FailureReason, cause error, verify func(protocol.GitOutcome) protocol.Verification) protocol.CompleteRequest {
	req.State, req.FailureReason = state, reason
	if cause != nil {
		a.emitter.Lifecycle("attempt failed", map[string]any{"reason": string(reason), "error": shortError(cause)})
		a.log.ErrorContext(ctx, "attempt failed", "reason", reason, "error", cause)
	}
	m := a.manifest
	if m == nil {
		req.Cleanup = protocol.Cleanup{Outcome: "missing", Reason: "worktree missing"}
		req.FinishedAt = a.r.clock().UTC()
		return req
	}
	// 7. git_inspect.
	span := a.emitter.StartSpan("git_inspect", "git_inspect", "", nil)
	git, err := a.r.git.Inspect(ctx, m.WorktreePath, m.BaseCommit)
	span.End(err, map[string]any{"dirty": git.Dirty, "commits": git.Commits, "pushed": git.Pushed})
	if err != nil {
		a.log.WarnContext(ctx, "git inspect failed", "error", err)
	}
	req.Git = git

	// 8. verify.
	if verify != nil {
		span = a.emitter.StartSpan("verify", "verify", "", nil)
		req.Verification = verify(git)
		span.End(nil, map[string]any{"level": req.Verification.Level, "passed": req.Verification.Passed, "reason": req.Verification.Reason})
	}

	// 9. cleanup.
	span = a.emitter.StartSpan("cleanup", "cleanup", "", nil)
	req.Cleanup = a.cleanup(ctx, state, git, err != nil)
	span.End(nil, map[string]any{"outcome": req.Cleanup.Outcome, "reason": req.Cleanup.Reason})
	req.FinishedAt = a.r.clock().UTC()
	return req
}

func (a *attempt) fail(req protocol.CompleteRequest, reason model.FailureReason, err error) protocol.CompleteRequest {
	req.State, req.FailureReason = model.Failed, reason
	req.Cleanup = protocol.Cleanup{Outcome: "missing", Reason: "worktree missing"}
	req.FinishedAt = a.r.clock().UTC()
	a.log.ErrorContext(a.ctx, "attempt failed before a worktree existed", "reason", reason, "error", err)
	return req
}

func (a *attempt) validate() error {
	c := a.claim
	for _, id := range []string{c.AttemptID, c.TargetID, c.WorkID} {
		if err := model.ValidateID(id); err != nil {
			return err
		}
	}
	if err := model.ValidateName(c.RoutineName); err != nil {
		return fmt.Errorf("routine: %w", err)
	}
	repo, ok := a.r.repos[c.Repository]
	if !ok {
		return fmt.Errorf("repository %s is not registered on this worker", c.Repository)
	}
	a.repo = repo
	if _, ok := a.r.executors[c.Executor]; !ok {
		return fmt.Errorf("executor %s is not configured", c.Executor)
	}
	if c.TimeoutSeconds <= 0 || c.TimeoutSeconds > 8*3600 {
		return fmt.Errorf("timeout %d out of range", c.TimeoutSeconds)
	}
	return nil
}

func (a *attempt) repoLock(name string) *sync.Mutex {
	v, _ := a.r.repoLocks.LoadOrStore(name, &sync.Mutex{})
	mu, ok := v.(*sync.Mutex)
	if !ok {
		panic("worker: repoLocks holds a non-mutex")
	}
	return mu
}

// prepareWorktree is steps 2–4: fetch, resolve_base, worktree_add, with the
// intent manifest written before the mutation.
func (a *attempt) prepareWorktree(ctx context.Context, wt, branch string) error {
	c := a.claim
	mu := a.repoLock(c.Repository)
	mu.Lock()
	defer mu.Unlock()
	g := a.r.git

	span := a.emitter.StartSpan("fetch", "fetch", "", nil)
	err := g.CheckOrigin(ctx, a.repo)
	var fetched bool
	var warn error
	if err == nil {
		base := a.repo.BaseBranch
		if base == "" {
			if b, _, rerr := g.ResolveBase(ctx, a.repo, ""); rerr == nil {
				base = b
			}
		}
		if base != "" {
			fetched, warn, err = g.Fetch(ctx, a.repo, base)
		}
	}
	if err == nil {
		err = g.CheckOrigin(ctx, a.repo)
	}
	attrs := map[string]any{"fetched": fetched}
	if warn != nil {
		attrs["fetch"] = "failed"
		attrs["warning"] = shortError(warn)
	}
	span.End(err, attrs)
	if err != nil {
		return fmt.Errorf("fetch %s: %w", c.Repository, err)
	}

	span = a.emitter.StartSpan("resolve_base", "resolve_base", "", nil)
	baseBranch, baseCommit, err := g.ResolveBase(ctx, a.repo, "")
	span.End(err, map[string]any{"branch": baseBranch, "commit": baseCommit})
	if err != nil {
		return fmt.Errorf("resolve base for %s: %w", c.Repository, err)
	}
	a.heartbeat(ctx, protocol.HeartbeatRequest{State: model.Preparing, Phase: "resolve_base"})

	span = a.emitter.StartSpan("worktree_add", "worktree_add", "", nil)
	if err := os.MkdirAll(filepath.Dir(wt), 0o700); err != nil {
		span.End(err, nil)
		return fmt.Errorf("create worktree root: %w", err)
	}
	m := &Manifest{
		AttemptID: c.AttemptID, TargetID: c.TargetID, WorkID: c.WorkID, RoutineName: c.RoutineName,
		RepositoryName: c.Repository, RepositoryPath: a.repo.Path, OriginIdentity: a.repo.OriginIdentity,
		BaseBranch: baseBranch, BaseCommit: baseCommit, WorktreePath: wt, Branch: branch, Kind: manifestKindAttempt,
		Lifecycle: ManifestPreparing,
	}
	if err = a.r.manifests.Write(m); err == nil {
		a.manifest = m
		err = g.WorktreeAdd(ctx, a.repo, wt, branch, baseCommit)
		if err != nil {
			// Never assume nothing happened: inspect and classify (DESIGN §5.4).
			st, serr := g.WorktreeState(ctx, a.repo, wt)
			switch {
			case serr == nil && st.PathExists && st.Registered:
				a.log.WarnContext(ctx, "worktree add reported failure but the worktree exists; continuing", "error", err)
				err = nil
			case serr == nil && (st.PathExists || st.Registered):
				m.Lifecycle, m.RetentionReason = ManifestInconsistent, "worktree creation left partial state: "+shortError(err)
				if werr := a.r.manifests.Write(m); werr != nil {
					err = errors.Join(err, werr)
				}
			}
		}
	}
	span.End(err, map[string]any{"path": wt, "branch": branch})
	if err != nil {
		return fmt.Errorf("worktree add for %s: %w", c.Repository, err)
	}
	return nil
}

func (a *attempt) writeMCPConfig() (string, error) {
	dir := filepath.Join(a.r.cfg.DataDir, "mcp")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", fmt.Errorf("create mcp dir: %w", err)
	}
	path := filepath.Join(dir, a.claim.AttemptID+".json")
	env := map[string]string{"FORGE_HOME": filepath.Dir(a.r.cfg.DataDir), "FORGE_SOCKET": strings.TrimPrefix(a.r.cfg.Daemon, "unix://"), "FORGE_TOKEN": a.claim.MCPToken}
	cfg := map[string]any{"mcpServers": map[string]any{"forge": map[string]any{"command": a.r.forgeBin, "args": []string{"mcp", "--attempt", a.claim.AttemptID}, "env": env}}}
	b, err := json.MarshalIndent(cfg, "", "  ")
	if err != nil {
		return "", fmt.Errorf("encode mcp config: %w", err)
	}
	if err := os.WriteFile(path, b, 0o600); err != nil {
		return "", fmt.Errorf("write mcp config: %w", err)
	}
	return path, nil
}

func (a *attempt) outputPath() string {
	return filepath.Join(a.r.cfg.DataDir, "output", a.claim.AttemptID+".log")
}

func (a *attempt) remaining(launches int) time.Duration {
	total := time.Duration(a.claim.TimeoutSeconds) * time.Second
	if a.manifest != nil && launches > 0 {
		total -= time.Duration(a.manifest.ElapsedBeforeUS) * time.Microsecond
	}
	if total < time.Second {
		total = time.Second
	}
	return total
}

func (a *attempt) env() []string {
	extra := []string{"FORGE_HOME=" + filepath.Dir(a.r.cfg.DataDir)}
	if len(a.claim.Policy.GitConfig) > 0 {
		extra = append(extra, GitConfigEnv(a.claim.Policy.GitConfig)...)
	}
	return PassthroughEnv(os.Environ(), extra...)
}

// prompt renders the executor's stdin: on a resume it is the human's answer.
func (a *attempt) prompt() string {
	if a.claim.Resume != nil {
		return a.claim.Resume.Answer
	}
	return a.claim.Prompt
}

func (a *attempt) promptVersion() *protocol.PromptVersion {
	c := a.claim
	if c.Resume != nil {
		return nil
	}
	h := promptHash(c.Mode, c.Prompt, "", c.AllowedTools, c.Model, c.Effort)
	return &protocol.PromptVersion{Hash: h, Routine: c.RoutineName, Generation: c.Generation, Mode: c.Mode, Template: c.Prompt, RenderedExample: c.Prompt, ToolList: c.AllowedTools, Model: c.Model, Effort: c.Effort}
}

// runAgent is phase 6: launch, stream, wait.
func (a *attempt) runAgent(ctx context.Context, launch int, mcpConfig string) (ParseResult, ExitStatus, error) {
	c := a.claim
	m := a.manifest
	exec := a.r.executors[c.Executor]
	spanID := fmt.Sprintf("agent-%d", launch)
	span := a.emitter.StartSpan(spanID, spanID, "", map[string]any{"executor": c.Executor, "model": c.Model, "launch": launch})
	factory, ok := a.r.parsers[exec.OutputParser()]
	if !ok {
		err := fmt.Errorf("parser %q is not registered", exec.OutputParser())
		span.End(err, nil)
		return ParseResult{}, ExitStatus{Code: -1}, err
	}
	parser := factory(spanID)
	sessionID := ""
	if c.Resume != nil {
		sessionID = c.Resume.SessionID
	}
	// The fake executor's fixture comes from the environment (tests and
	// `just smoke` set it); production executors ignore the variable.
	fixture := os.Getenv("FORGE_FAKE_FIXTURE")
	cmd, err := exec.Command(ctx, LaunchRequest{
		Model: c.Model, MaxTurns: c.MaxTurns, Repo: c.Repository, Worktree: m.WorktreePath, MCPConfig: mcpConfig,
		SessionID: sessionID, Fixture: fixture, AllowedTools: c.AllowedTools, MaxBudgetUSD: c.MaxBudgetUSD, Effort: c.Effort, Env: a.env(),
	})
	if err != nil {
		span.End(err, nil)
		return ParseResult{}, ExitStatus{Code: -1}, err
	}
	if err := os.MkdirAll(filepath.Dir(a.outputPath()), 0o700); err != nil {
		span.End(err, nil)
		return ParseResult{}, ExitStatus{Code: -1}, fmt.Errorf("create output dir: %w", err)
	}
	openSpans := map[string]int64{}
	var openMu sync.Mutex
	onLine := func(line []byte) {
		for _, ev := range parser.Line(line) {
			switch ev.Kind {
			case protocol.KindSpanStart:
				openMu.Lock()
				openSpans[ev.SpanID] = a.emitter.Elapsed()
				openMu.Unlock()
				a.emitter.Emit(ev)
			case protocol.KindSpanEnd:
				openMu.Lock()
				started, ok := openSpans[ev.SpanID]
				delete(openSpans, ev.SpanID)
				openMu.Unlock()
				if ok {
					a.emitter.EndTool(ev, started)
				} else {
					a.emitter.Emit(ev)
				}
			default:
				a.emitter.Emit(ev)
			}
		}
		a.log.Log(a.ctx, logging.LevelTrace, "executor stdout", "line", string(line))
	}
	onErr := func(line []byte) {
		a.emitter.Emit(protocol.Event{Kind: protocol.KindStderr, Message: string(line)})
		a.log.Log(a.ctx, logging.LevelTrace, "executor stderr", "line", string(line))
	}
	p, err := Launch(ctx, LaunchSpec{Cmd: cmd, Prompt: a.prompt(), Timeout: a.remaining(launch - 1), OutputPath: a.outputPath(), OnStdout: onLine, OnStderr: onErr})
	if err != nil {
		span.End(err, nil)
		return ParseResult{}, ExitStatus{Code: -1}, err
	}
	a.mu.Lock()
	a.process = p
	stop := a.stopReason
	a.mu.Unlock()
	if stop != "" {
		// A cancel arrived while launching: honour it now.
		if serr := p.Stop(stop, killGrace); serr != nil {
			a.log.WarnContext(ctx, "stop after launch", "error", serr)
		}
	}
	m.PID, m.PIDStart, m.ProcessActive, m.Lifecycle, m.Launches = p.PID(), p.PIDStart(), true, ManifestRunning, launch
	if err := a.r.manifests.Write(m); err != nil {
		a.log.ErrorContext(ctx, "manifest write after launch", "error", err)
	}
	a.heartbeat(ctx, protocol.HeartbeatRequest{State: model.Running, Phase: "agent", PID: p.PID(), PIDStart: p.PIDStart()})
	a.emitter.Lifecycle("agent started", map[string]any{"pid": p.PID(), "launch": launch})
	exit := p.Wait()
	result := parser.Result()
	m.ProcessActive, m.Lifecycle, m.SessionID = false, ManifestExited, result.SessionID
	m.ElapsedBeforeUS, m.NextSeq = a.emitter.Elapsed(), a.emitter.NextSeq()
	if err := a.r.manifests.Write(m); err != nil {
		a.log.ErrorContext(ctx, "manifest write after exit", "error", err)
	}
	for _, s := range result.Samples {
		a.emitter.Metric("rate_limit_sample", map[string]any{"window": s.Window, "utilization": s.Utilization, "resets_at": s.ResetsAt.Unix()})
	}
	span.End(exit.Err, map[string]any{"exit_code": exit.Code, "timed_out": exit.TimedOut, "stopped": exit.Stopped, "turns": result.NumTurns, "session_id": result.SessionID, "unknown_lines": result.UnknownLines, "dropped_lines": result.DroppedLines})
	a.emitter.Lifecycle("agent exited", map[string]any{"exit_code": exit.Code, "signal": exit.Signal, "timed_out": exit.TimedOut})
	return result, exit, nil
}

// cleanup is phase 9: decide, act, record on the manifest.
func (a *attempt) cleanup(ctx context.Context, state model.State, git protocol.GitOutcome, inspectFailed bool) protocol.Cleanup {
	m := a.manifest
	st, err := a.r.git.WorktreeState(ctx, a.repo, m.WorktreePath)
	if err != nil {
		m.Lifecycle, m.RetentionReason = ManifestRetained, "worktree state unknown: "+shortError(err)
		a.writeManifest(ctx, m)
		return protocol.Cleanup{Outcome: "retained", Reason: m.RetentionReason, Command: a.cleanupCommand()}
	}
	d := DecideCleanup(CleanupInput{
		Resumable: state == model.WaitingHuman, AwaitingMerge: a.claim.Integrate && state == model.Succeeded,
		PathExists: st.PathExists, Registered: st.Registered, Dirty: git.Dirty || inspectFailed,
		HeadIsBase: git.Head == m.BaseCommit, Pushed: git.Pushed, AttemptShortID: model.ShortID(a.claim.AttemptID),
	})
	m.Resumable = state == model.WaitingHuman
	switch d.Action {
	case CleanupKeep:
		if d.Reason == "awaiting merge" {
			m.Lifecycle = ManifestAwaitingMerge
		} else {
			m.Lifecycle = ManifestExited
		}
		a.writeManifest(ctx, m)
		return protocol.Cleanup{Outcome: "kept", Reason: d.Reason}
	case CleanupMissing:
		m.Lifecycle = ManifestMissing
		a.writeManifest(ctx, m)
		return protocol.Cleanup{Outcome: "missing", Reason: d.Reason}
	case CleanupRetain:
		m.Lifecycle, m.RetentionReason, m.CleanupCommand = ManifestRetained, d.Reason, d.Command
		if d.Inconsistent {
			m.Lifecycle = ManifestInconsistent
		}
		a.writeManifest(ctx, m)
		return protocol.Cleanup{Outcome: "retained", Reason: d.Reason, Command: d.Command}
	}
	mu := a.repoLock(a.claim.Repository)
	mu.Lock()
	defer mu.Unlock()
	m.CleanupIntent = cleanupIntentAutomatic
	a.writeManifest(ctx, m)
	if err := a.r.git.WorktreeRemove(ctx, a.repo, m.WorktreePath, false); err != nil {
		m.Lifecycle, m.RetentionReason, m.CleanupCommand = ManifestRetained, "removal failed: "+shortError(err), a.cleanupCommand()
		a.writeManifest(ctx, m)
		return protocol.Cleanup{Outcome: "retained", Reason: m.RetentionReason, Command: m.CleanupCommand}
	}
	m.Lifecycle = ManifestCleaned
	a.writeManifest(ctx, m)
	return protocol.Cleanup{Outcome: "removed", Reason: "removed"}
}

func (a *attempt) cleanupCommand() string {
	return fmt.Sprintf("forge cleanup %s --confirm", model.ShortID(a.claim.AttemptID))
}

func (a *attempt) writeManifest(ctx context.Context, m *Manifest) {
	if err := a.r.manifests.Write(m); err != nil {
		a.log.ErrorContext(ctx, "manifest write", "lifecycle", m.Lifecycle, "error", err)
	}
}

// report sends complete, retrying on transport errors while the lease could
// still be alive; a definitive daemon answer ends it.
func (a *attempt) report(ctx context.Context, req protocol.CompleteRequest) {
	delay := heartbeatRetryMin
	deadline := time.Now().Add(leaseLostAfter)
	for {
		resp, err := a.r.daemon.Complete(ctx, a.claim.AttemptID, req)
		if err == nil {
			a.log.InfoContext(ctx, "attempt completed", "state", resp.State, "late", resp.Late, "reason", req.FailureReason, "cleanup", req.Cleanup.Outcome)
			return
		}
		var se *StatusError
		if errors.As(err, &se) && se.Status < 500 {
			a.log.ErrorContext(ctx, "daemon refused completion", "status", se.Status, "message", se.Message)
			return
		}
		if time.Now().After(deadline) {
			a.log.ErrorContext(ctx, "could not report completion; reconcile will retry", "error", err)
			return
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(delay):
		}
		if delay *= 2; delay > heartbeatRetryMax {
			delay = heartbeatRetryMax
		}
	}
}

// heartbeat sends one heartbeat now (phase changes) and applies the answer.
func (a *attempt) heartbeat(ctx context.Context, hb protocol.HeartbeatRequest) {
	hb.LeaseToken = leaseTokenFor(a.claim)
	resp, err := a.r.daemon.Heartbeat(ctx, a.claim.AttemptID, hb)
	if err != nil {
		a.log.DebugContext(ctx, "heartbeat failed", "phase", hb.Phase, "error", err)
		return
	}
	a.applyHeartbeat(resp)
}

func (a *attempt) applyHeartbeat(resp *protocol.HeartbeatResponse) {
	if resp.CancelRequested {
		a.stop("cancelled")
	}
}

// heartbeatLoop runs from claim until complete: it renews the lease every 10 s,
// backs off on failure, and declares the lease lost after 120 s without success.
func (a *attempt) heartbeatLoop(ctx context.Context) {
	lastOK := time.Now()
	delay := heartbeatInterval
	for {
		select {
		case <-ctx.Done():
			return
		case <-time.After(delay):
		}
		resp, err := a.r.daemon.Heartbeat(ctx, a.claim.AttemptID, protocol.HeartbeatRequest{LeaseToken: leaseTokenFor(a.claim), Phase: "heartbeat"})
		switch {
		case err == nil:
			lastOK = time.Now()
			delay = heartbeatInterval
			a.applyHeartbeat(resp)
		default:
			var se *StatusError
			if errors.As(err, &se) && se.Status == 409 {
				a.log.WarnContext(ctx, "lease no longer held", "message", se.Message)
				a.stop("lease_lost")
				return
			}
			if time.Since(lastOK) > leaseLostAfter {
				a.log.WarnContext(ctx, "no successful heartbeat for 120 s; stopping the agent", "error", err)
				a.stop("lease_lost")
				return
			}
			if delay < heartbeatRetryMax {
				delay = min(delay*2, heartbeatRetryMax)
			}
			if delay == heartbeatInterval {
				delay = heartbeatRetryMin
			}
		}
	}
}

func (a *attempt) stop(reason string) {
	a.mu.Lock()
	if a.stopReason == "" {
		a.stopReason = reason
	}
	p := a.process
	a.mu.Unlock()
	if p != nil {
		if err := p.Stop(reason, killGrace); err != nil {
			a.log.WarnContext(a.ctx, "stop", "reason", reason, "error", err)
		}
	}
}

func (a *attempt) stopped() string {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.stopReason
}

// promptHash is the PromptVersion key: templates, system append, sorted tool
// list, model, effort — never the rendered context (MODES.md, DESIGN.md §9.1).
func promptHash(mode, template, systemAppend string, tools []string, model, effort string) string {
	sorted := append([]string(nil), tools...)
	sort.Strings(sorted)
	h := sha256.New()
	for _, part := range []string{mode, template, systemAppend, strings.Join(sorted, ","), model, effort} {
		h.Write([]byte(part))
		h.Write([]byte{0})
	}
	return hex.EncodeToString(h.Sum(nil))
}

// leaseTokenFor is the token minted at claim, carried on the claim so every
// request can present it.
func leaseTokenFor(c *protocol.Claim) string { return c.LeaseToken }
