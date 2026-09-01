package controlplane

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"forge/internal/core/logging"
	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/store"
)

// sampleLookback is how far before an attempt's start the facts computation
// looks for the "before" rate-limit sample.
const sampleLookback = 6 * time.Hour

func (s *Server) register(r *http.Request) (int, any, error) {
	ctx := r.Context()
	var req protocol.RegisterRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	if err := model.ValidateID(req.WorkerID); err != nil {
		return 0, nil, badRequest("worker id: %v", err)
	}
	if err := model.ValidateName(req.Name); err != nil {
		return 0, nil, badRequest("worker name: %v", err)
	}
	for _, repo := range req.Repositories {
		if err := model.ValidateName(repo.Name); err != nil {
			return 0, nil, badRequest("repository: %v", err)
		}
	}
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		if err := tx.Register(ctx, req); err != nil {
			return err
		}
		if err := s.proposeForgeToml(ctx, tx, req.Repositories); err != nil {
			return err
		}
		return tx.TouchWorker(ctx, req.WorkerID)
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.DebugContext(ctx, "worker registered", "worker_id", req.WorkerID, "name", req.Name, "repositories", len(req.Repositories), "active", req.Active, "max_concurrent", req.MaxConcurrent)
	return http.StatusOK, protocol.RegisterResponse{LogLevels: s.currentLogLevels()}, nil
}

func (s *Server) claim(r *http.Request) (int, any, error) {
	ctx := r.Context()
	var req protocol.ClaimRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	if err := model.ValidateID(req.WorkerID); err != nil {
		return 0, nil, badRequest("worker id: %v", err)
	}
	if req.ClaimRequestID == "" || req.LeaseToken == "" {
		return 0, nil, badRequest("claim_request_id and lease_token are required")
	}
	if s.Draining() {
		return 0, nil, errDraining
	}
	// The dependency graph and the routines' concurrency limits are read before
	// the transaction: neither has a transactional reader. An edge or a limit
	// changed between here and the pick is seen by the next claim, a few seconds
	// later; the pick itself and the claim are atomic.
	edges, err := s.store.DependencyEdges(ctx)
	if err != nil {
		return 0, nil, err
	}
	routines, err := s.store.ListRoutines(ctx, true)
	if err != nil {
		return 0, nil, err
	}
	concurrency := make(map[string]int, len(routines))
	for _, rt := range routines {
		concurrency[rt.ID] = rt.Concurrency
	}
	var claim *protocol.Claim
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		claim, err = s.claimTx(ctx, tx, req, edges, concurrency)
		return err
	})
	if err != nil {
		return 0, nil, err
	}
	if claim == nil {
		return http.StatusNoContent, nil, nil
	}
	return http.StatusOK, claim, nil
}

// claimTx is DESIGN.md §5 step 0: inside one transaction, read the queue's
// inputs, pick the first admissible Target for this worker, and claim it.
func (s *Server) claimTx(ctx context.Context, tx *store.Tx, req protocol.ClaimRequest, edges []model.Edge, concurrency map[string]int) (*protocol.Claim, error) {
	workers, err := tx.Workers(ctx)
	if err != nil {
		return nil, err
	}
	var worker *store.Worker
	for i := range workers {
		if workers[i].ID == req.WorkerID {
			worker = &workers[i]
		}
	}
	if worker == nil {
		return nil, fmt.Errorf("worker %s is not registered: %w", req.WorkerID, store.ErrNotFound)
	}
	work, err := tx.OpenWork(ctx)
	if err != nil {
		return nil, err
	}
	targets, err := tx.OpenTargets(ctx)
	if err != nil {
		return nil, err
	}
	if replay, err := s.replayedClaim(ctx, tx, req, targets); err != nil || replay != nil {
		return replay, err
	}
	repos, err := tx.Repositories(ctx)
	if err != nil {
		return nil, err
	}
	byName := make(map[string]store.Repository, len(repos))
	for _, repo := range repos {
		byName[repo.Name] = repo
	}
	active, err := tx.ActiveWorkByRoutine(ctx)
	if err != nil {
		return nil, err
	}
	finished, err := s.finishedStates(ctx, work, edges)
	if err != nil {
		return nil, err
	}
	leases, err := tx.PathLeases(ctx)
	if err != nil {
		return nil, err
	}
	order := Order(QueueInput{Work: work, Targets: groupTargets(targets), Edges: edges, Deferred: s.deferred, FinishedStates: finished})
	pick := Pick(PickInput{Order: order, Worker: *worker, Repositories: byName, Concurrency: concurrency, Active: active,
		Requirements: workRequirements, Leases: toPathLeases(leases), Edges: edges, MaxStackDepth: s.maxStackDepth, LeaseExempt: s.leaseExempt})
	// A path_lease skip starts the lease_wait_us clock (facts): journal the
	// first refusal per Target, inside this transaction.
	for id, reason := range pick.Skipped {
		if holder, ok := strings.CutPrefix(reason, "path_lease held by "); ok {
			if err := tx.MarkLeaseBlocked(ctx, id, holder); err != nil {
				return nil, err
			}
		}
	}
	if pick.Target == nil {
		s.log.DebugContext(ctx, "nothing to claim", "worker_id", req.WorkerID, "open_work", len(work), "skipped", SortedSkips(pick.Skipped))
		return nil, nil
	}
	return s.claimTarget(ctx, tx, req, *worker, *pick.Work, *pick.Target, edges)
}

// leaseExempt reports Work whose mode does not write the repository
// (WritesNone, WritesKbOnly): verify and plan attempts neither take nor wait
// on path leases. An unknown mode (or no registry) is treated as writing —
// the conservative side.
func (s *Server) leaseExempt(w store.Work) bool {
	snap, err := snapshotRoutine(w)
	if err != nil {
		return false
	}
	return s.modeWritesNothing(snap.Mode)
}

// modeWritesNothing reports a registered mode whose write scope cannot touch
// the repository. An unknown mode (or no registry) counts as writing.
func (s *Server) modeWritesNothing(mode string) bool {
	if s.modes == nil {
		return false
	}
	m := s.modes.Get(mode)
	if m == nil {
		return false
	}
	switch m.Writes() {
	case model.WritesNone, model.WritesKbOnly:
		return true
	}
	return false
}

// toPathLeases adapts the store rows to the scheduler's input type.
func toPathLeases(rows []store.PathLease) []PathLease {
	out := make([]PathLease, len(rows))
	for i, l := range rows {
		out[i] = PathLease{TargetID: l.TargetID, Repository: l.Repository, Globs: l.Globs}
	}
	return out
}

// workRequirements is the capability-routing rule at the claim site: a
// require_sandbox routine runs only on a worker whose sandbox capability is
// ready, and a verify Work whose subject needs L2 UI verification only on one
// whose browser is ready. Everything else needs nothing extra (M10 runners
// arrive through the same seam).
func workRequirements(w store.Work) []string {
	snap, err := snapshotRoutine(w)
	if err != nil {
		return nil
	}
	var reqs []string
	if snap.RequireSandbox {
		reqs = append(reqs, "sandbox")
	}
	if snap.Mode == "verify" {
		if vo := verifyOfFromSnapshot(w.Snapshot); vo != nil && vo.UI {
			reqs = append(reqs, "browser")
		}
	}
	return reqs
}

// proposeForgeToml files one doc proposal per registered repository that has
// no forge.toml (M11 bootstrap, DESIGN §22): a suggested skeleton with checks
// guessed from the checkout. It fires at most once per repository — any
// existing proposal for the target, whatever a human decided, suppresses a
// new one — and only for checkouts the daemon can see (V0: daemon and worker
// share a machine; an invisible path yields nothing to draft from).
func (s *Server) proposeForgeToml(ctx context.Context, tx *store.Tx, repos []protocol.Repository) error {
	rows, err := tx.Repositories(ctx)
	if err != nil {
		return err
	}
	// A worker already reported a forge.toml for these (the file may live
	// off-disk of this daemon's view); never propose over one.
	reported := map[string]bool{}
	for _, row := range rows {
		if row.ForgeToml != "" {
			reported[row.Name] = true
		}
	}
	for _, r := range repos {
		if r.Name == "greenfield" || reported[r.Name] {
			// The virtual greenfield repository is not a checkout.
			continue
		}
		if info, err := os.Stat(r.Path); err != nil || !info.IsDir() {
			continue
		}
		if _, err := os.Stat(filepath.Join(r.Path, "forge.toml")); err == nil {
			continue
		}
		target := "repo:" + r.Name + "/forge.toml"
		exists, err := tx.HasProposalForTarget(ctx, target)
		if err != nil {
			return err
		}
		if exists {
			continue
		}
		after, err := json.Marshal(map[string]string{"forge_toml": forgeTomlSkeleton(r.Path)})
		if err != nil {
			return fmt.Errorf("encode forge.toml skeleton: %w", err)
		}
		p := &store.Proposal{
			Source: "daemon:register", Kind: model.ProposalDoc, Target: target, After: after,
			Rationale:        fmt.Sprintf("repository %s has no forge.toml; without one Forge cannot run declared checks (L1). The skeleton guesses checks from the checkout.", r.Name),
			VerificationPlan: "human reviews and commits forge.toml to the repository; forge_check validates the declared checks on the next attempt",
		}
		if err := tx.CreateProposal(ctx, p); err != nil {
			return err
		}
		s.log.InfoContext(ctx, "forge.toml proposal created", "repository", r.Name, "proposal_id", p.ID)
	}
	return nil
}

// forgeTomlSkeleton drafts a forge.toml for a checkout: checks guessed from
// package.json scripts (lint/build/test → npm run X) and go.mod (go test),
// else an empty [checks] table for the human to fill.
func forgeTomlSkeleton(path string) string {
	var b strings.Builder
	b.WriteString("# Proposed by Forge (M11 bootstrap): review, adjust, commit.\n[checks]\n")
	declared := map[string]bool{}
	if raw, err := os.ReadFile(filepath.Join(path, "package.json")); err == nil {
		var pkg struct {
			Scripts map[string]string `json:"scripts"`
		}
		if json.Unmarshal(raw, &pkg) == nil {
			for _, name := range []string{"lint", "build", "test"} {
				if _, ok := pkg.Scripts[name]; ok && !declared[name] {
					fmt.Fprintf(&b, "%s = [\"npm\", \"run\", %q]\n", name, name)
					declared[name] = true
				}
			}
		}
	}
	if _, err := os.Stat(filepath.Join(path, "go.mod")); err == nil && !declared["test"] {
		b.WriteString("test = [\"go\", \"test\", \"./...\"]\n")
		declared["test"] = true
	}
	if len(declared) == 0 {
		b.WriteString("# declare checks: name = [\"cmd\", \"arg\", ...]\n")
	}
	return b.String()
}

// replayedClaim answers a retried claim whose first response was lost (DESIGN.md
// §14): the worker already holds a leased Target whose attempt carries this
// claim_request_id. The attempt row was committed by the first claim, so the
// reader pool sees it. The mcp_token cannot be returned again — only its hash
// is stored — so a fresh one is minted and rebound (RebindMCPToken) inside the
// transaction, so forge mcp can present the token this response carries.
func (s *Server) replayedClaim(ctx context.Context, tx *store.Tx, req protocol.ClaimRequest, targets []store.Target) (*protocol.Claim, error) {
	for _, t := range targets {
		if t.WorkerID != req.WorkerID || !model.Leased(t.State) {
			continue
		}
		a, err := s.store.AttemptForTarget(ctx, t.ID)
		if err != nil {
			return nil, err
		}
		if a == nil || a.ClaimRequestID != req.ClaimRequestID {
			continue
		}
		w, err := tx.GetWork(ctx, t.WorkID)
		if err != nil {
			return nil, err
		}
		token, err := newToken()
		if err != nil {
			return nil, err
		}
		if err := tx.RebindMCPToken(ctx, a.ID, token); err != nil {
			return nil, err
		}
		s.log.InfoContext(ctx, "claim replayed", "attempt_id", a.ID, "target_id", t.ID, "worker_id", req.WorkerID)
		claim, _, err := s.claimResponse(ctx, tx, *w, t, a, token)
		return claim, err
	}
	return nil, nil
}

// claimTarget records the claim and the queue_wait span, then renders the
// worker's frozen view of the Work.
func (s *Server) claimTarget(ctx context.Context, tx *store.Tx, req protocol.ClaimRequest, worker store.Worker, w store.Work, t store.Target, edges []model.Edge) (*protocol.Claim, error) {
	snap, err := snapshotRoutine(w)
	if err != nil {
		return nil, err
	}
	// M10 routing (DESIGN.md §21): choose the model, runner, and escalation
	// rung before any state change. A nil choice means no runner can run a
	// candidate right now — leave the Target pending and let the worker retry.
	choice, err := s.routeClaim(ctx, worker, w, t, snap, tx)
	if err != nil {
		return nil, err
	}
	if choice == nil {
		s.log.DebugContext(ctx, "no runner available for target", "target_id", t.ID, "routine", w.RoutineName)
		return nil, nil
	}
	token, err := newToken()
	if err != nil {
		return nil, err
	}
	stack, err := s.stackBase(ctx, tx, w, t, edges)
	if err != nil {
		return nil, err
	}
	params := store.ClaimParams{
		TargetID: t.ID, WorkerID: req.WorkerID, ClaimRequestID: req.ClaimRequestID, LeaseToken: req.LeaseToken, MCPToken: token,
		Executor: choice.Executor, Model: choice.ModelID, ModelAlias: choice.Alias, Runner: choice.Runner,
		EscalatedFrom: choice.EscalatedFrom, Routing: choice.RoutingJSON, Effort: snap.Effort, Mode: snap.Mode, Autonomy: w.Autonomy,
	}
	if !s.leaseExempt(w) {
		params.Globs = EffectiveGlobs(w.Paths, w.Deps)
	}
	if stack != nil {
		params.StackBase = stack.Commit
	}
	a, err := tx.Claim(ctx, params)
	if err != nil {
		return nil, err
	}
	ctx = logging.ContextWith(ctx, slog.String("attempt_id", a.ID), slog.String("target_id", t.ID), slog.String("work_id", w.ID))
	claim, answered, err := s.claimResponse(ctx, tx, w, t, a, token)
	if err != nil {
		return nil, err
	}
	if stack != nil {
		claim.StackBase = stack
	}
	// queue_wait is the one span the control plane records: created (or, on a
	// resume, answered) to claimed, on its own wall clock (DESIGN.md §5 step 0).
	from := w.CreatedAt
	if answered != nil {
		from = answered.AnsweredAt
	}
	span := protocol.Event{
		Seq: a.Launches, Time: tx.Now(), Kind: protocol.KindSpanEnd, Name: "queue_wait", SpanID: fmt.Sprintf("queue_wait-%d", a.Launches),
		DurationUS: max(tx.Now().Sub(from).Microseconds(), 0), Attrs: json.RawMessage(`{"clock":"wall"}`),
	}
	if _, err := tx.InsertEvents(ctx, a.ID, protocol.SourceControl, []protocol.Event{span}); err != nil {
		return nil, err
	}
	s.log.InfoContext(ctx, "claimed", "worker_id", req.WorkerID, "routine", w.RoutineName, "repository", t.Repository, "resume", claim.Resume != nil, "queue_wait_us", span.DurationUS)
	return claim, nil
}

// claimResponse renders protocol.Claim for an attempt, with Resume set when the
// attempt has an answered question to continue from; that question is returned
// too so the caller can measure the wait since its answer.
func (s *Server) claimResponse(ctx context.Context, tx *store.Tx, w store.Work, t store.Target, a *store.Attempt, mcpToken string) (*protocol.Claim, *store.Question, error) {
	snap, err := snapshotRoutine(w)
	if err != nil {
		return nil, nil, err
	}
	claim := &protocol.Claim{
		AttemptID: a.ID, TargetID: t.ID, WorkID: w.ID, RoutineName: w.RoutineName, Generation: w.Generation, Repository: t.Repository,
		Mode: snap.Mode, Executor: a.Executor, Model: a.Model, Effort: a.Effort,
		MaxTurns: snap.MaxTurns, TimeoutSeconds: snap.TimeoutSeconds, MaxBudgetUSD: snap.MaxBudgetUSD, AllowedTools: snap.AllowedTools,
		Autonomy: a.Autonomy, BudgetClass: w.BudgetClass, Trigger: w.Trigger, LeaseExpiresAt: tx.Now().Add(store.LeaseDuration), Integrate: w.Integrate,
		MCPToken: mcpToken, Policy: protocol.Policy{RequireSandbox: snap.RequireSandbox, AllowHosts: s.allowHosts, GitConfig: s.gitConfig}, Snapshot: w.Snapshot,
	}
	claim.PromptTemplate, claim.Prompt = s.assembleClaimPrompt(ctx, tx, snap, t, a)
	claim.SystemAppend = s.repoBrief(ctx, t.Repository)
	claim.VerifyOf = verifyOfFromSnapshot(w.Snapshot)
	claim.ModeInfo = s.claimModeInfo(ctx, tx, snap.Mode, t.Repository)
	if a.StackBaseCommit != "" {
		// A replayed or resumed stacked claim: the pinned base commit is on
		// the attempt row; branch and depth matter only for the first
		// prepare, which already happened.
		claim.StackBase = &protocol.StackBase{Commit: a.StackBaseCommit}
	}
	q, err := tx.LastAnswer(ctx, a.ID)
	if err != nil {
		return nil, nil, err
	}
	if q != nil {
		claim.Resume = &protocol.Resume{SessionID: a.SessionID, Answer: q.Answer, Launches: a.Launches}
	}
	return claim, q, nil
}

// stackBase resolves the base a stacked claim starts on (DESIGN.md §20): when
// the Work has a stack_on edge to a dependency that has succeeded but not yet
// merged, the claim is pinned to that dependency's task-branch head. A merged
// (or plainly succeeded, non-integrating) dependency means the ordinary base
// already contains its work. The dependency's attempt row is read through the
// pool: it was committed when that attempt completed.
func (s *Server) stackBase(ctx context.Context, tx *store.Tx, w store.Work, t store.Target, edges []model.Edge) (*protocol.StackBase, error) {
	var dep string
	for _, e := range edges {
		if e.Work == w.ID && e.StackOn && e.On == model.OnSuccess {
			dep = e.BlockedBy
			break
		}
	}
	if dep == "" {
		return nil, nil
	}
	dw, err := tx.GetWork(ctx, dep)
	if err != nil {
		return nil, fmt.Errorf("stack dependency of %s: %w", w.ID, err)
	}
	dts, err := s.store.TargetsForWork(ctx, dw.ID)
	if err != nil {
		return nil, err
	}
	for _, dt := range dts {
		if dt.Repository != t.Repository {
			continue
		}
		switch dt.State {
		case model.Succeeded, model.QueuedForMerge, model.Merging, model.Conflict:
		default:
			return nil, nil // merged (or not integrating): the ordinary base carries it
		}
		da, err := s.store.AttemptForTarget(ctx, dt.ID)
		if err != nil {
			return nil, err
		}
		if da == nil || da.HeadCommit == "" || da.Branch == "" {
			return nil, fmt.Errorf("stack base of %s: dependency %s has no recorded branch head", w.ID, dep)
		}
		return &protocol.StackBase{WorkID: dep, Branch: da.Branch, Commit: da.HeadCommit, Depth: StackDepth(w.ID, edges)}, nil
	}
	return nil, nil
}

// snapshotRoutine decodes the Work's frozen routine; ad-hoc Work stores the same
// shape, so one decoder serves both.
func snapshotRoutine(w store.Work) (store.Routine, error) {
	var r store.Routine
	if err := json.Unmarshal(w.Snapshot, &r); err != nil {
		return r, fmt.Errorf("decode snapshot of work %s: %w", w.ID, err)
	}
	return r, nil
}

// newToken mints a per-attempt MCP token: 32 random bytes, hex.
func newToken() (string, error) {
	var b [32]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", fmt.Errorf("mint token: %w", err)
	}
	return hex.EncodeToString(b[:]), nil
}

// deferred adapts the policy to Order's question ("is this class deferred?").
func (s *Server) deferred(class model.BudgetClass) (bool, string) {
	admit, reason := s.policy.Decide(class)
	return !admit, reason
}

func groupTargets(ts []store.Target) map[string][]store.Target {
	out := map[string][]store.Target{}
	for _, t := range ts {
		out[t.WorkID] = append(out[t.WorkID], t)
	}
	return out
}

// finishedStates derives the state of every dependency that is no longer open
// (Order only sees open Work). Reads go through the pool: finished Work is,
// by definition, not being changed by the transaction in progress.
func (s *Server) finishedStates(ctx context.Context, open []store.Work, edges []model.Edge) (map[string]model.WorkState, error) {
	openIDs := make(map[string]bool, len(open))
	for _, w := range open {
		openIDs[w.ID] = true
	}
	out := map[string]model.WorkState{}
	for _, e := range edges {
		if openIDs[e.BlockedBy] {
			continue
		}
		if _, done := out[e.BlockedBy]; done {
			continue
		}
		w, err := s.store.GetWork(ctx, e.BlockedBy)
		if errors.Is(err, store.ErrNotFound) {
			continue // Dependencies treats a missing state as failed
		}
		if err != nil {
			return nil, err
		}
		ts, err := s.store.TargetsForWork(ctx, w.ID)
		if err != nil {
			return nil, err
		}
		out[w.ID] = model.DeriveWorkState(model.WorkInputs{Targets: targetStates(ts), Integrate: w.Integrate})
	}
	return out, nil
}

// attemptRequest reads the {id} of an attempt route and stamps it on the
// context so every log line of the request carries attempt_id.
func attemptRequest(r *http.Request) (context.Context, string, error) {
	id, err := pathID(r)
	if err != nil {
		return nil, "", err
	}
	return logging.ContextWith(r.Context(), slog.String("attempt_id", id)), id, nil
}

func (s *Server) heartbeat(r *http.Request) (int, any, error) {
	ctx, id, err := attemptRequest(r)
	if err != nil {
		return 0, nil, err
	}
	var req protocol.HeartbeatRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	if req.LeaseToken == "" {
		return 0, nil, badRequest("lease_token is required")
	}
	if req.State != "" && req.State != model.Preparing && req.State != model.Running {
		return 0, nil, badRequest("heartbeat state %q: want preparing or running", req.State)
	}
	var resp protocol.HeartbeatResponse
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		a, err := tx.GetAttempt(ctx, id)
		if err != nil {
			return err
		}
		t, err := tx.RecordHeartbeat(ctx, id, req)
		if err != nil {
			return err
		}
		// Surface the reported state/phase on the live progress tally so the task
		// view can show what the running attempt is doing right now.
		if err := tx.RecordHeartbeatProgress(ctx, id, req.State, req.Phase); err != nil {
			return err
		}
		cancel, expires, err := tx.Heartbeat(ctx, t.ID, req.LeaseToken)
		if err != nil {
			return err
		}
		steers, err := tx.TakeSteers(ctx, id)
		if err != nil {
			return err
		}
		resp = protocol.HeartbeatResponse{CancelRequested: cancel, LeaseExpiresAt: expires, LogLevels: s.currentLogLevels(), Steer: steers}
		// Deliver any supervisor-adjudicated budget grant on this heartbeat. The
		// grants ride the journal (audit == queue, like steers); the newest one
		// wins for the response's single slot, and its bounded amount is what the
		// worker applies to the effective budget.
		grants, err := tx.TakeBudgetGrants(ctx, id)
		if err != nil {
			return err
		}
		if len(grants) > 0 {
			g := grants[len(grants)-1]
			resp.GrantedBudget = &protocol.GrantedBudget{Dimension: g.Dimension, Amount: g.Amount}
			resp.Nudge = g.Nudge
		}
		return tx.TouchWorker(ctx, a.WorkerID)
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.DebugContext(ctx, "heartbeat", "phase", req.Phase, "state", req.State, "pid", req.PID, "cancel_requested", resp.CancelRequested)
	return http.StatusOK, resp, nil
}

func (s *Server) postEvents(r *http.Request) (int, any, error) {
	ctx, id, err := attemptRequest(r)
	if err != nil {
		return 0, nil, err
	}
	var batch protocol.EventBatch
	if err := decodeJSON(r, &batch); err != nil {
		return 0, nil, err
	}
	switch batch.Source {
	case protocol.SourceWorker, protocol.SourceMCP, protocol.SourceControl:
	default:
		return 0, nil, badRequest("source %q: want worker, mcp, or control", batch.Source)
	}
	if len(batch.Events) > protocol.MaxEventBatch {
		return 0, nil, badRequest("batch of %d events exceeds %d", len(batch.Events), protocol.MaxEventBatch)
	}
	for _, e := range batch.Events {
		if err := e.Validate(); err != nil {
			return 0, nil, badRequest("%v", err)
		}
	}
	samples, err := rateLimitSamples(id, batch.Events)
	if err != nil {
		return 0, nil, err
	}
	var inserted int
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		if _, err := tx.GetAttempt(ctx, id); err != nil {
			return err
		}
		n, err := tx.InsertEvents(ctx, id, batch.Source, batch.Events)
		if err != nil {
			return err
		}
		inserted = n
		// Refresh the live progress tally (turns/tokens/last-event/note) from the
		// events just landed, so a running attempt's task view is current.
		if n > 0 {
			if err := tx.RecomputeAttemptProgress(ctx, id); err != nil {
				return err
			}
		}
		// A resets_at change against the latest stored sample is a window reset;
		// journal its unspent headroom before the new samples land (§10.2).
		if err := s.journalBudgetResets(ctx, tx, samples); err != nil {
			return err
		}
		return tx.InsertSamples(ctx, samples)
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.DebugContext(ctx, "events stored", "source", batch.Source, "received", len(batch.Events), "inserted", inserted, "samples", len(samples))
	return http.StatusOK, map[string]int{"inserted": inserted}, nil
}

// rateLimitAttrs is the shape of a rate_limit metric's attrs; the windows are
// optional so a parser that saw only one still records it.
type rateLimitAttrs struct {
	FiveHourUtilization *float64 `json:"five_hour_utilization"`
	FiveHourResetsAt    int64    `json:"five_hour_resets_at"`
	SevenDayUtilization *float64 `json:"seven_day_utilization"`
	SevenDayResetsAt    int64    `json:"seven_day_resets_at"`
}

// rateLimitSamples turns rate_limit metrics into RateLimitSample rows.
func rateLimitSamples(attemptID string, events []protocol.Event) ([]store.RateLimitSample, error) {
	var out []store.RateLimitSample
	for _, e := range events {
		if e.Kind != protocol.KindMetric || e.Name != "rate_limit" || len(e.Attrs) == 0 {
			continue
		}
		var attrs rateLimitAttrs
		if err := json.Unmarshal(e.Attrs, &attrs); err != nil {
			return nil, badRequest("event %d: rate_limit attrs: %v", e.Seq, err)
		}
		if attrs.FiveHourUtilization != nil {
			out = append(out, store.RateLimitSample{Time: e.Time, Window: "five_hour", Utilization: *attrs.FiveHourUtilization, ResetsAt: time.Unix(attrs.FiveHourResetsAt, 0).UTC(), SourceAttempt: attemptID})
		}
		if attrs.SevenDayUtilization != nil {
			out = append(out, store.RateLimitSample{Time: e.Time, Window: "seven_day", Utilization: *attrs.SevenDayUtilization, ResetsAt: time.Unix(attrs.SevenDayResetsAt, 0).UTC(), SourceAttempt: attemptID})
		}
	}
	return out, nil
}

func (s *Server) complete(r *http.Request) (int, any, error) {
	ctx, id, err := attemptRequest(r)
	if err != nil {
		return 0, nil, err
	}
	var req protocol.CompleteRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	if req.LeaseToken == "" {
		return 0, nil, badRequest("lease_token is required")
	}
	switch req.State {
	case model.Succeeded, model.WaitingHuman, model.Failed, model.Cancelled:
	default:
		return 0, nil, badRequest("state %q: want succeeded, waiting_human, failed, or cancelled", req.State)
	}
	if req.State == model.WaitingHuman && req.Question == nil {
		return 0, nil, badRequest("waiting_human needs a question")
	}
	var out *store.CompleteOutcome
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		a, err := tx.GetAttempt(ctx, id)
		if err != nil {
			return err
		}
		if sweptBefore(a) {
			out, err = s.lateCompletion(ctx, tx, a, req)
			return err
		}
		if req.State == model.WaitingHuman {
			if err := s.enforceAskBudget(ctx, tx, a, &req); err != nil {
				return err
			}
		}
		out, err = tx.Complete(ctx, id, req, s.modeRequiredLevel(a.Mode))
		if err != nil {
			return err
		}
		work, err := tx.GetWork(ctx, out.Target.WorkID)
		if err != nil {
			return err
		}
		if !out.Late && !out.Again {
			// M4 verification orchestration: worker verdict row, artifacts,
			// verify-attempt verdicts, follow-up Work (handlers_verify.go).
			if err := s.afterComplete(ctx, tx, a, work, out.Target, req); err != nil {
				return err
			}
		}
		// Facts are computed at attempt-terminal (DESIGN.md §9.2): for an
		// integrating Work that is `succeeded`, before the Target moves on
		// into the merge queue; the integrator later fills the merge columns.
		if model.IsTerminal(out.Target.State, work.Integrate) || (work.Integrate && out.Target.State == model.Succeeded) {
			if err := s.recordFacts(ctx, tx, id); err != nil {
				return err
			}
		}
		if err := s.enqueueMerge(ctx, tx, work, out.Target.ID); err != nil {
			return err
		}
		// The response reports where the Target actually landed (it may have
		// just entered the merge queue).
		out.Target, err = tx.GetTarget(ctx, out.Target.ID)
		return err
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "attempt completed", "reported", req.State, "state", out.Target.State, "late", out.Late, "again", out.Again, "exit_code", req.ExitCode, "turns", req.NumTurns)
	return http.StatusOK, protocol.CompleteResponse{State: out.Target.State, Late: out.Late}, nil
}

// enforceAskBudget is M11's max_questions rule (DESIGN §22): when a new
// question would exceed the routine's budget, the report is rewritten to fail
// the Target with ask_budget_exhausted before tx.Complete runs, so the
// ordinary completion path records the failure and its facts. The refusal is
// journaled on the Target. Auto and notify autonomy never report
// waiting_human, so they are untouched by construction.
func (s *Server) enforceAskBudget(ctx context.Context, tx *store.Tx, a *store.Attempt, req *protocol.CompleteRequest) error {
	t, err := tx.GetTarget(ctx, a.TargetID)
	if err != nil {
		return err
	}
	w, err := tx.GetWork(ctx, t.WorkID)
	if err != nil {
		return err
	}
	snap, err := snapshotRoutine(*w)
	if err != nil {
		return err
	}
	if snap.MaxQuestions <= 0 {
		return nil // no budget declared (follow-up Work snapshots, old rows)
	}
	asked, err := tx.QuestionCount(ctx, t.ID)
	if err != nil {
		return err
	}
	if asked < snap.MaxQuestions {
		return nil
	}
	if err := tx.Journal(ctx, "question.budget_exhausted", store.EntityTarget, t.ID, map[string]any{"attempt_id": a.ID, "asked": asked, "max": snap.MaxQuestions}); err != nil {
		return err
	}
	s.log.InfoContext(ctx, "ask budget exhausted", "target_id", t.ID, "asked", asked, "max", snap.MaxQuestions)
	req.State, req.FailureReason, req.Question = model.Failed, model.ReasonAskBudgetExhausted, nil
	return nil
}

// sweptBefore recognises an attempt the sweeper closed: finished as
// lease_expired with no cleanup outcome, which only a worker reports. The
// store's Complete sees finished_at and answers "again" for such an attempt,
// so the late path of DESIGN.md §5 step 10 is taken here instead.
func sweptBefore(a *store.Attempt) bool {
	return !a.FinishedAt.IsZero() && a.FailureReason == model.ReasonLeaseExpired && a.Cleanup.Outcome == ""
}

// lateCompletion lands only the git and cleanup fields of a completion for a
// Target the sweeper already failed, and journals that nothing else was kept.
// The facts row was written by the sweep; it stays as it is.
func (s *Server) lateCompletion(ctx context.Context, tx *store.Tx, a *store.Attempt, req protocol.CompleteRequest) (*store.CompleteOutcome, error) {
	t, err := tx.GetTarget(ctx, a.TargetID)
	if err != nil {
		return nil, err
	}
	git := req.Git
	if err := tx.PatchCleanup(ctx, a.ID, protocol.CleanupPatch{Git: &git, Cleanup: req.Cleanup}); err != nil {
		return nil, err
	}
	if err := tx.Journal(ctx, "attempt.late_completion", store.EntityAttempt, a.ID, map[string]any{"state": req.State, "target_state": t.State}); err != nil {
		return nil, err
	}
	return &store.CompleteOutcome{Target: t, Late: true}, nil
}

// recordFacts computes and inserts the facts row for a terminal attempt inside
// the transaction that made it terminal. The attempt, Target, and Work rows come
// from the transaction (they were just changed); events, questions, and samples
// come from the reader pool, which is complete for them because they are only
// ever written in their own, earlier transactions. Facts are immutable, so an
// existing row (an idempotent retry, a sweep racing a completion) is left alone.
func (s *Server) recordFacts(ctx context.Context, tx *store.Tx, attemptID string) error {
	a, err := tx.GetAttempt(ctx, attemptID)
	if err != nil {
		return err
	}
	t, err := tx.GetTarget(ctx, a.TargetID)
	if err != nil {
		return err
	}
	w, err := tx.GetWork(ctx, t.WorkID)
	if err != nil {
		return err
	}
	project, err := tx.ProjectForRepository(ctx, t.Repository)
	if err != nil {
		return err
	}
	events, err := s.store.Events(ctx, a.ID, true, 0)
	if err != nil {
		return err
	}
	all, err := s.store.QuestionsForWork(ctx, w.ID)
	if err != nil {
		return err
	}
	var questions []store.Question
	for _, q := range all {
		if q.AttemptID == a.ID {
			questions = append(questions, q)
		}
	}
	since := a.StartedAt
	if since.IsZero() {
		since = tx.Now()
	}
	since = since.Add(-sampleLookback)
	fiveHour, err := s.store.SamplesSince(ctx, "five_hour", since)
	if err != nil {
		return err
	}
	sevenDay, err := s.store.SamplesSince(ctx, "seven_day", since)
	if err != nil {
		return err
	}
	leaseBlockedAt, err := s.store.LeaseBlockedAt(ctx, t.ID)
	if err != nil {
		return err
	}
	info, _ := s.modelInfoFor(a.ModelAlias)
	facts := ComputeFacts(FactsInput{Attempt: *a, Target: *t, Work: *w, Project: project.Name, Events: events, Questions: questions, Samples: append(fiveHour, sevenDay...), Now: tx.Now(), LeaseBlockedAt: leaseBlockedAt, Model: info})
	if err := tx.InsertFacts(ctx, facts); err != nil {
		if errors.Is(err, store.ErrConflict) {
			s.log.DebugContext(ctx, "facts already recorded", "attempt_id", a.ID)
			return nil
		}
		return err
	}
	s.log.InfoContext(ctx, "facts recorded", "attempt_id", a.ID, "target_id", t.ID, "work_id", w.ID, "state", facts.State, "events", len(events))
	return nil
}

func (s *Server) cleanup(r *http.Request) (int, any, error) {
	ctx, id, err := attemptRequest(r)
	if err != nil {
		return 0, nil, err
	}
	var patch protocol.CleanupPatch
	if err := decodeJSON(r, &patch); err != nil {
		return 0, nil, err
	}
	if patch.Cleanup.Outcome == "" {
		return 0, nil, badRequest("cleanup.outcome is required")
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.PatchCleanup(ctx, id, patch) }); err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "cleanup patched", "outcome", patch.Cleanup.Outcome, "reason", patch.Cleanup.Reason)
	return http.StatusNoContent, nil, nil
}

// workerAttemptState mirrors worker.AttemptState (the worker package cannot be
// imported here; the JSON shape is the contract).
type workerAttemptState struct {
	AttemptID   string      `json:"attempt_id"`
	TargetID    string      `json:"target_id"`
	TargetState model.State `json:"target_state"`
	Terminal    bool        `json:"terminal"`
	Resumable   bool        `json:"resumable"`
}

func (s *Server) workerAttempt(r *http.Request) (int, any, error) {
	ctx, id, err := attemptRequest(r)
	if err != nil {
		return 0, nil, err
	}
	a, err := s.store.GetAttempt(ctx, id)
	if err != nil {
		return 0, nil, err
	}
	t, err := s.store.GetTarget(ctx, a.TargetID)
	if err != nil {
		return 0, nil, err
	}
	work, err := s.store.GetWork(ctx, t.WorkID)
	if err != nil {
		return 0, nil, err
	}
	resumable := t.WorkerID != "" && (t.State == model.WaitingHuman || t.State == model.Pending)
	return http.StatusOK, workerAttemptState{AttemptID: a.ID, TargetID: t.ID, TargetState: t.State, Terminal: model.IsTerminal(t.State, work.Integrate), Resumable: resumable}, nil
}
