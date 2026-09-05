package web

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strconv"
	"strings"
	"time"

	"forge/internal/core/engine"
	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

// Ad-hoc Work defaults (POST /api/v1/tasks without a routine).
const (
	adHocRoutineName  = "ad-hoc"
	adHocMode         = "run"
	adHocModel        = "haiku"
	adHocExecutor     = "claude-code"
	adHocTimeout      = 1800
	adHocMaxTurns     = 30
	adHocPriority     = 100
	adHocMaxQuestions = 3
	titleMaxRunes     = 80

	// Directive workflow-node envelope defaults (node config overrides).
	directiveNodeTimeout  = 3600
	directiveNodeMaxTurns = 60
	directiveNodePriority = 50
)

// routineWriteError maps what CreateRoutine and UpdateRoutine return: the
// sentinels keep their status; anything else came from Validate, which runs
// before the row is touched, so it is the client's (a driver failure on the
// insert itself would be misreported as 400; its message still names it).
func routineWriteError(err error) error {
	if err == nil || errors.Is(err, store.ErrNotFound) || errors.Is(err, store.ErrConflict) || errors.Is(err, store.ErrStaleGeneration) {
		return err
	}
	return badRequest("%v", err)
}

func (s *Server) listRoutines(r *http.Request) (int, any, error) {
	routines, err := s.store.ListRoutines(r.Context(), r.URL.Query().Get("archived") == "true")
	if err != nil {
		return 0, nil, err
	}
	if routines == nil {
		routines = []store.Routine{}
	}
	return http.StatusOK, routines, nil
}

// decodeRoutine reads a routine body and rejects what the API decides before
// the store does: the name, and a model alias the daemon cannot resolve.
func (s *Server) decodeRoutine(r *http.Request) (*store.Routine, error) {
	var rt store.Routine
	if err := decodeJSON(r, &rt); err != nil {
		return nil, err
	}
	rt.ID, rt.Generation = "", 0
	if err := model.ValidateName(rt.Name); err != nil {
		return nil, badRequest("%v", err)
	}
	kind, target, err := store.ParseTarget(rt.Target)
	if err != nil || kind == "" {
		return nil, badRequest("routine %s: target is required (directive:<name> or workflow:<name>)", rt.Name)
	}
	if rt.Mode != "" || rt.Prompt != "" || rt.Persona != "" || rt.Model != "" || rt.Effort != "" {
		return nil, badRequest("routine %s: a routine carries no content fields — the directive owns mode/prompt/persona/model/effort", rt.Name)
	}
	switch kind {
	case store.TargetDirective:
		// The library may be absent (tests, a bare server) — then the name
		// is taken on faith and run creation is where a mistake surfaces.
		if lib := s.promptLibrary(); lib != nil && lib.Directive(target) == nil {
			return nil, badRequest("routine %s: directive %q is not in the library (directives/%s.md)", rt.Name, target, target)
		}
	case store.TargetWorkflow:
		if _, err := s.store.GetWorkflow(r.Context(), target); err != nil {
			return nil, badRequest("routine %s: workflow %q: %v", rt.Name, target, err)
		}
	case store.TargetScript:
		if lib := s.promptLibrary(); lib != nil && lib.Script(target) == nil {
			return nil, badRequest("routine %s: script %q is not in the library (scripts/%s.js)", rt.Name, target, target)
		}
	}
	return &rt, nil
}

func (s *Server) createRoutine(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	rt, err := s.decodeRoutine(r)
	if err != nil {
		return 0, nil, err
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return routineWriteError(tx.CreateRoutine(ctx, rt)) }); err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "routine created", "routine", rt.Name, "routine_id", rt.ID)
	return http.StatusCreated, rt, nil
}

func (s *Server) getRoutine(r *http.Request) (int, any, error) {
	rt, err := s.store.GetRoutine(r.Context(), r.PathValue("name"))
	if err != nil {
		return 0, nil, err
	}
	return http.StatusOK, rt, nil
}

func (s *Server) updateRoutine(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	generation, err := strconv.Atoi(r.URL.Query().Get("generation"))
	if err != nil || generation <= 0 {
		return 0, nil, badRequest("generation query parameter is required: the generation you edited")
	}
	rt, err := s.decodeRoutine(r)
	if err != nil {
		return 0, nil, err
	}
	if rt.Name != r.PathValue("name") {
		return 0, nil, badRequest("routine name %q does not match the path", rt.Name)
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		saved, err := tx.GetRoutine(ctx, rt.Name)
		if err != nil {
			return err
		}
		rt.ID = saved.ID // the generation record is keyed by it
		return routineWriteError(tx.UpdateRoutine(ctx, rt, generation))
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "routine updated", "routine", rt.Name, "generation", rt.Generation)
	return http.StatusOK, rt, nil
}

func (s *Server) archiveRoutine(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	name := r.PathValue("name")
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.ArchiveRoutine(ctx, name) }); err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "routine archived", "routine", name)
	return http.StatusNoContent, nil, nil
}

// runRequest is POST /api/v1/routines/{name}/run's optional body.
type runRequest struct {
	Repositories []string `json:"repositories"`
	Objective    string   `json:"objective"`
}

func (s *Server) runRoutine(r *http.Request) (int, any, error) {
	ctx := r.Context()
	var body runRequest
	if r.ContentLength != 0 {
		if err := decodeJSON(r, &body); err != nil {
			return 0, nil, err
		}
	}
	name := r.PathValue("name")
	rt, err := s.store.GetRoutine(ctx, name)
	if err != nil {
		return 0, nil, err
	}
	// A workflow- or script-target routine runs as a run, not a Work — the
	// routine is the trigger, its repositories/objective the run context.
	switch kind, target := targetOf(rt); kind {
	case store.TargetWorkflow:
		if s.Draining() {
			return 0, nil, errDraining
		}
		run, err := s.startRoutineWorkflowRun(ctx, rt, target, firstNonEmptySlice(body.Repositories, rt.Repositories), firstNonEmpty(body.Objective, rt.Objective), model.TriggerManual)
		if err != nil {
			return 0, nil, err
		}
		s.log.InfoContext(ctx, "routine fired workflow run", "routine", rt.Name, "workflow", target, "run_id", run.ID)
		return http.StatusCreated, workflowRunCreated{RunID: run.ID, Workflow: target, Generation: run.WorkflowGeneration}, nil
	case store.TargetScript:
		if s.Draining() {
			return 0, nil, errDraining
		}
		run, err := s.startScriptRun(ctx, target, firstNonEmptySlice(body.Repositories, rt.Repositories), firstNonEmpty(body.Objective, rt.Objective), model.TriggerManual)
		if err != nil {
			return 0, nil, err
		}
		s.log.InfoContext(ctx, "routine fired script run", "routine", rt.Name, "script", target, "run_id", run.ID)
		return http.StatusCreated, workflowRunCreated{RunID: run.ID, Workflow: "script:" + target}, nil
	}
	return s.submitWork(ctx, workRequest{Routine: name, Repositories: body.Repositories, Objective: body.Objective})
}

// startScriptRun runs one library script through the flow engine as a
// synthetic single-node run: the runs page, cancel, skip-if-running, and the
// journal all come for free. The run's name is "script:<name>"; it belongs
// to no workflow row.
func (s *Server) startScriptRun(ctx context.Context, script string, repos []string, objective string, trigger model.Trigger) (*store.WorkflowRun, error) {
	if lib := s.promptLibrary(); lib == nil || lib.Script(script) == nil {
		return nil, badRequest("script %q is not in the library (scripts/%s.js)", script, script)
	}
	run := &store.WorkflowRun{
		WorkflowName: "script:" + script, Trigger: trigger,
		Context: store.RunContext{Repositories: repos, Objective: objective},
		Graph: &store.WorkflowGraph{Nodes: []store.WorkflowNode{{
			ID: "main", Type: store.NodeScript, Config: map[string]any{"script": script},
		}}},
	}
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		if err := tx.CreateWorkflowRun(ctx, run); err != nil {
			return err
		}
		return tx.Journal(ctx, "workflow.run_created", store.EntityWorkflow, run.ID, map[string]any{"script": script, "trigger": run.Trigger})
	})
	if err != nil {
		return nil, err
	}
	s.advanceRun(ctx, run.ID)
	return run, nil
}

// startRoutineWorkflowRun creates a workflow run on a trigger routine's
// behalf and advances it — the runWorkflow/fireDueWorkflows shape.
func (s *Server) startRoutineWorkflowRun(ctx context.Context, rt *store.Routine, workflow string, repos []string, objective string, trigger model.Trigger) (*store.WorkflowRun, error) {
	run := &store.WorkflowRun{Trigger: trigger, Context: store.RunContext{Repositories: repos, Objective: objective}}
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		wf, err := tx.GetWorkflow(ctx, workflow)
		if err != nil {
			return err
		}
		if !wf.ArchivedAt.IsZero() {
			return badRequest("routine %s targets archived workflow %s", rt.Name, wf.Name)
		}
		run.WorkflowID, run.WorkflowName, run.WorkflowGeneration, run.Graph = wf.ID, wf.Name, wf.Generation, wf.Graph
		if err := tx.CreateWorkflowRun(ctx, run); err != nil {
			return err
		}
		return tx.Journal(ctx, "workflow.run_created", store.EntityWorkflow, run.ID, map[string]any{"workflow": wf.Name, "generation": wf.Generation, "trigger": run.Trigger, "routine": rt.Name, "nodes": len(wf.Graph.Nodes)})
	})
	if err != nil {
		return nil, err
	}
	s.advanceRun(ctx, run.ID)
	return run, nil
}

func firstNonEmptySlice(a, b []string) []string {
	if len(a) > 0 {
		return a
	}
	return b
}

// workRequest is POST /api/v1/work|tasks: a routine run with overrides, or an
// ad-hoc task built from the fields.
type workRequest struct {
	Prompt       string            `json:"prompt"`
	Repositories []string          `json:"repositories"`
	Mode         string            `json:"mode"`
	Routine      string            `json:"routine"`
	Priority     *int              `json:"priority"`
	Class        model.BudgetClass `json:"class"`
	Autonomy     model.Autonomy    `json:"autonomy"`
	// Size is the optional S|M|L bucket for this ask; trusted when given (no
	// sizing gate), frozen on the Work, copied into facts for calibration.
	Size string `json:"size"`
	// BenchName tags a benchmark run's root: submitted_by becomes
	// "bench:<name>" so `forge bench list` finds its history.
	BenchName string   `json:"bench_name"`
	Model     string   `json:"model"`
	After     []string `json:"after"`
	Paths     []string `json:"paths"`
	Integrate bool     `json:"integrate"`
	Title     string   `json:"title"`
	// MaxTurns and TimeoutSeconds override the ad-hoc defaults (haiku/30 turns/
	// 1800s) for an ad-hoc task; ignored when a routine is named. Pointers so
	// "unset" is distinct from 0.
	MaxTurns       *int `json:"max_turns"`
	TimeoutSeconds *int `json:"timeout_seconds"`
	// Objective, when set, is substituted for {{objective}} in the routine
	// prompt at creation (per-work, like {{repo}} is per-repo at claim). Empty
	// leaves a self-directed fallback. Threaded from a workflow/routine run.
	Objective string `json:"objective"`
	// Force overrides intake dedupe (M11): submit even when an identical
	// prompt was created within the window.
	Force bool `json:"force"`
	// Persona names an identity from the prompts library composed ahead of
	// the prompt; it overrides the routine's persona for this run.
	Persona string `json:"persona"`
	// CausedBy chains this submission's intent explicitly to an existing Work
	// (DESIGN.md §3 "Provenance"): the store makes it the caused_by parent and
	// inherits its root. Empty leaves the Work a root. For CLI/plugins/future
	// callers; a manual or routine run leaves it unset.
	CausedBy string `json:"caused_by"`

	// Workflow stamps and prebuilt step edges, set only by the run engine —
	// unexported so a request body can never forge them.
	workflowRunID string
	workflowName  string
	workflowStep  string
	stepEdges     []model.Edge
	// directive materializes a Work straight from a directives-library file —
	// a directive workflow node, no stored routine involved. nodeTimeout and
	// nodeMaxTurns are its operational envelope (0 = engine defaults).
	directive    string
	nodeTimeout  int
	nodeMaxTurns int
	// cause + submittedBy override the manual defaults for engine- and
	// tool-created Works ("tool" + "agent:<attempt>"); unexported so a
	// request body can never forge provenance.
	cause       model.Cause
	submittedBy string
	// trigger overrides the manual default: the engine stamps a scheduled
	// run's Works `schedule` so analytics can tell them apart. Unexported for
	// the same reason.
	trigger model.Trigger
}

// workCreated is the 201 body.
type workCreated struct {
	Work    store.Work     `json:"work"`
	Targets []store.Target `json:"targets"`
}

func (s *Server) createWork(r *http.Request) (int, any, error) {
	var req workRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	return s.submitWork(r.Context(), req)
}

func (s *Server) submitWork(ctx context.Context, req workRequest) (int, any, error) {
	if s.Draining() {
		return 0, nil, errDraining
	}
	if err := s.dedupeWork(ctx, req); err != nil {
		return 0, nil, err
	}
	var out workCreated
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		var err error
		out, err = s.createWorkTx(ctx, tx, req)
		return err
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "work created", "work_id", out.Work.ID, "routine", out.Work.RoutineName, "generation", out.Work.Generation, "targets", len(out.Targets), "priority", out.Work.Priority, "class", out.Work.BudgetClass)
	return http.StatusCreated, out, nil
}

// injectObjective substitutes a routine prompt's {{objective}} placeholder with
// the run's objective (per-work, baked at creation). An empty objective becomes
// a self-directed fallback, so a flow works with or without one; a prompt with
// no placeholder is unchanged.
func injectObjective(prompt, objective string) string {
	if !strings.Contains(prompt, "{{objective}}") {
		return prompt
	}
	obj := strings.TrimSpace(objective)
	if obj == "" {
		obj = "(No specific objective was given — identify the single highest-value next piece of work for this repository yourself, and state clearly what you chose and why.)"
	}
	return strings.ReplaceAll(prompt, "{{objective}}", obj)
}

// createWorkTx freezes the routine (or the ad-hoc fields as a routine) into a
// Work snapshot and creates its Targets. Repositories must be registered;
// `after` ids must exist; the model alias must resolve.
func (s *Server) createWorkTx(ctx context.Context, tx *store.Tx, req workRequest) (workCreated, error) {
	var rt store.Routine
	var w store.Work
	if req.Routine != "" {
		saved, err := tx.GetRoutine(ctx, req.Routine)
		if err != nil {
			return workCreated{}, err
		}
		if !saved.ArchivedAt.IsZero() {
			return workCreated{}, fmt.Errorf("routine %s is archived: %w", saved.Name, store.ErrConflict)
		}
		if kind, target := targetOf(saved); kind == store.TargetWorkflow {
			return workCreated{}, badRequest("routine %s targets workflow %q: run it as a workflow (POST /api/v1/routines/%s/run), not as a Work", saved.Name, target, saved.Name)
		}
		rt = *saved
		if len(req.Paths) > 0 {
			rt.Paths = req.Paths
		}
		rt.Integrate = rt.Integrate || req.Integrate
		w = store.Work{RoutineID: saved.ID, RoutineName: saved.Name, Generation: saved.Generation, Tier: saved.Tier, Models: saved.Models, Deps: saved.Deps}
	} else if req.directive != "" {
		// A directive workflow node: the content comes from the library, the
		// operational envelope from the node config (engine defaults where
		// unset). No stored routine row exists or is created.
		rt = store.Routine{
			Name: req.directive, Target: "directive:" + req.directive,
			Repositories: req.Repositories, Executor: adHocExecutor,
			TimeoutSeconds: directiveNodeTimeout, MaxTurns: directiveNodeMaxTurns,
			BudgetClass: model.ClassNormal, Priority: directiveNodePriority, Concurrency: 1,
			Paths: req.Paths, Integrate: req.Integrate, MaxQuestions: adHocMaxQuestions,
		}
		if req.nodeTimeout > 0 {
			rt.TimeoutSeconds = req.nodeTimeout
		}
		if req.nodeMaxTurns > 0 {
			rt.MaxTurns = req.nodeMaxTurns
		}
		w = store.Work{RoutineName: req.directive}
	} else {
		if strings.TrimSpace(req.Prompt) == "" {
			return workCreated{}, badRequest("prompt is required")
		}
		rt = store.Routine{
			Name: adHocRoutineName, Mode: adHocMode, Prompt: req.Prompt, Repositories: req.Repositories, Executor: adHocExecutor, Model: adHocModel,
			TimeoutSeconds: adHocTimeout, MaxTurns: adHocMaxTurns, BudgetClass: model.ClassInteractive, Priority: adHocPriority, Concurrency: 1,
			Paths: req.Paths, Integrate: req.Integrate, MaxQuestions: adHocMaxQuestions,
		}
		if req.MaxTurns != nil && *req.MaxTurns > 0 {
			rt.MaxTurns = *req.MaxTurns
		}
		if req.TimeoutSeconds != nil && *req.TimeoutSeconds > 0 {
			rt.TimeoutSeconds = *req.TimeoutSeconds
		}
		w = store.Work{RoutineName: adHocRoutineName}
	}
	composition, err := s.materializeRoutine(&rt, materializeOpts{
		Mode: req.Mode, Model: req.Model, Prompt: req.Prompt, Persona: req.Persona, Objective: req.Objective,
		// Root works only: continuations and follow-ups must not switch
		// arms mid-task (live_experiments.go).
		Assign: req.CausedBy == "",
	})
	if err != nil {
		return workCreated{}, err
	}
	if req.Class != "" {
		if !req.Class.Valid() {
			return workCreated{}, badRequest("class %q: want interactive, normal, or backlog", req.Class)
		}
		rt.BudgetClass = req.Class
	}
	if req.Priority != nil {
		rt.Priority = *req.Priority
	}
	if req.Autonomy != "" && !req.Autonomy.Valid() {
		return workCreated{}, badRequest("autonomy %q: want ask, checkpoint, notify, or auto", req.Autonomy)
	}
	if !store.ValidSize(req.Size) {
		return workCreated{}, badRequest("size %q: want S, M, or L", req.Size)
	}
	w.Size = req.Size
	if _, ok := s.resolveModel(rt.Model); !ok {
		return workCreated{}, badRequest("unknown model alias %q", rt.Model)
	}
	repos := req.Repositories
	if len(repos) == 0 {
		repos = rt.Repositories
	}
	if len(repos) == 0 {
		return workCreated{}, badRequest("at least one repository is required")
	}
	if len(repos) > protocol.MaxRepositoriesWork {
		return workCreated{}, badRequest("%d repositories exceed %d", len(repos), protocol.MaxRepositoriesWork)
	}
	registered, err := tx.Repositories(ctx)
	if err != nil {
		return workCreated{}, err
	}
	known := make(map[string]bool, len(registered))
	for _, repo := range registered {
		known[repo.Name] = true
	}
	for i, repo := range repos {
		if known[repo] {
			continue
		}
		// Repositories on the fly (DESIGN §1.3): resolve a name under
		// projects_root or an absolute path, append it to worker.toml, and
		// record a provisional row; the worker advertises it on its next
		// registration tick (≤30s) and scheduling proceeds from there.
		if s.registerRepo == nil {
			return workCreated{}, fmt.Errorf("repository %s is not registered: %w", repo, store.ErrNotFound)
		}
		rep, err := s.registerRepo(ctx, repo)
		if err != nil {
			return workCreated{}, fmt.Errorf("repository %s is not registered and could not be added (%v): %w", repo, err, store.ErrNotFound)
		}
		if err := tx.UpsertProvisionalRepository(ctx, rep); err != nil {
			return workCreated{}, err
		}
		s.log.InfoContext(ctx, "repository registered on the fly", "repository", rep.Name, "path", rep.Path)
		repos[i] = rep.Name
		known[rep.Name] = true
	}
	project, err := tx.ProjectForRepository(ctx, repos[0])
	if err != nil {
		return workCreated{}, err
	}
	var edges []model.Edge
	for _, id := range req.After {
		if err := model.ValidateID(id); err != nil {
			return workCreated{}, badRequest("after: %v", err)
		}
		if _, err := tx.GetWork(ctx, id); err != nil {
			return workCreated{}, err
		}
		edges = append(edges, model.Edge{BlockedBy: id, On: model.OnSuccess})
	}
	edges = append(edges, req.stepEdges...)
	if req.CausedBy != "" {
		if err := model.ValidateID(req.CausedBy); err != nil {
			return workCreated{}, badRequest("caused_by: %v", err)
		}
		if _, err := tx.GetWork(ctx, req.CausedBy); err != nil {
			return workCreated{}, err
		}
		w.CausedByWorkID, w.Cause = req.CausedBy, model.CauseFollowUp
		if req.cause != "" {
			w.Cause = req.cause
		}
	}
	rt.Repositories = repos
	snapshot, err := json.Marshal(rt)
	if err != nil {
		return workCreated{}, fmt.Errorf("snapshot routine: %w", err)
	}
	w.Title = req.Title
	if w.Title == "" {
		w.Title = titleFromPrompt(rt.Prompt, rt.Name)
	}
	w.PromptHash = promptHashOf(rt.Prompt)
	w.Persona = rt.Persona
	if composition != nil {
		raw, err := json.Marshal(composition)
		if err != nil {
			return workCreated{}, fmt.Errorf("encode composition: %w", err)
		}
		w.Composition = raw
	}
	w.Trigger, w.Snapshot, w.Priority, w.BudgetClass = model.TriggerManual, snapshot, rt.Priority, rt.BudgetClass
	if req.trigger != "" {
		w.Trigger = req.trigger
	}
	w.WorkflowRunID, w.WorkflowName, w.WorkflowStep = req.workflowRunID, req.workflowName, req.workflowStep
	w.Autonomy = model.ResolveAutonomy(req.Autonomy, rt.Autonomy, "", project.Autonomy, "")
	w.Integrate, w.Paths, w.SubmittedBy = rt.Integrate, rt.Paths, "human"
	if req.submittedBy != "" {
		w.SubmittedBy = req.submittedBy
	}
	if req.BenchName != "" {
		if err := model.ValidateName(req.BenchName); err != nil {
			return workCreated{}, badRequest("bench_name: %v", err)
		}
		w.SubmittedBy = "bench:" + req.BenchName
	}
	if w.Integrate && s.modeWritesNothing(rt.Mode) {
		// A non-writing mode (plan, verify) produces nothing to merge: the
		// Work-level flag off keeps succeeded terminal, while the snapshot
		// keeps integrate = true as the batch-inheritance hint planFollowUps
		// reads (DESIGN.md §20).
		w.Integrate = false
	}
	if len(rt.Deps) > 0 {
		w.Deps = rt.Deps
	}
	// The learning ledger (DESIGN: [learning] usd_per_week): self-improvement
	// roots — reflect-library runs and scratch promotions — draw from their
	// own pool, never a project's. Only roots are gated: a mid-flight child
	// (verify follow-up, continuation) must not wedge on an exhausted pool.
	if req.CausedBy == "" && s.learningCfg.BudgetUSDPerWeek > 0 &&
		(rt.Name == "reflect-library" || req.cause == model.CausePromotion) {
		spent, err := s.store.LearningSpendSince(ctx, s.now().Add(-7*24*time.Hour))
		if err != nil {
			return workCreated{}, err
		}
		if spent >= s.learningCfg.BudgetUSDPerWeek {
			if err := tx.Journal(ctx, "learning.budget_exhausted", store.EntityDaemon, "", map[string]any{
				"spent_usd": spent, "budget_usd": s.learningCfg.BudgetUSDPerWeek, "routine": rt.Name}); err != nil {
				return workCreated{}, err
			}
			return workCreated{}, fmt.Errorf("learning budget exhausted ($%.2f of $%.2f this week): %w", spent, s.learningCfg.BudgetUSDPerWeek, store.ErrConflict)
		}
	}
	if w.Integrate && len(w.Deps) > 0 {
		// M9 known gap: the serialized deps pre-step (a Forge-authored
		// lockfile commit through the merge queue, DESIGN.md §20) is not
		// implemented. Refusing loudly beats silently integrating without it.
		return workCreated{}, badRequest("deps = [...] with integrate = true is not implemented (M9 known gap: the serialized dependency pre-step is missing); drop deps or integrate")
	}
	targets, err := tx.CreateWork(ctx, &w, repos, edges)
	if err != nil {
		return workCreated{}, err
	}
	return workCreated{Work: w, Targets: targets}, nil
}

// dedupeWindow is how long an identical ad-hoc prompt is refused (M11 intake
// dedupe, DESIGN §22). Wall clock across requests by necessity.
const dedupeWindow = 24 * time.Hour

// dedupeWork refuses (409, journaled work.deduplicated) an ad-hoc submission
// whose normalized prompt was already submitted within the window; force
// overrides. Routine runs are deliberate reruns of a stable prompt and are
// not deduplicated. The read is on the pool and the journal row is its own
// transaction because a refusal must not roll it back; the race window is one
// request and --force exists, so advisory is enough. The external_refs half
// of the spec is skipped: the create body carries no external_refs today.
func (s *Server) dedupeWork(ctx context.Context, req workRequest) error {
	if req.Routine != "" || req.Force {
		return nil
	}
	prompt := strings.TrimSpace(req.Prompt)
	if prompt == "" {
		return nil // createWorkTx rejects it with the better message
	}
	hash := promptHashOf(prompt)
	dups, err := s.store.WorkByPromptHash(ctx, hash, s.now().UTC().Add(-dedupeWindow))
	if err != nil {
		return err
	}
	if len(dups) == 0 {
		return nil
	}
	dup := dups[0]
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		return tx.Journal(ctx, "work.deduplicated", store.EntityWork, dup.ID, map[string]any{"prompt_hash": hash, "duplicate_of": dup.ID})
	})
	if err != nil {
		return err
	}
	s.log.InfoContext(ctx, "work deduplicated", "duplicate_of", dup.ID, "prompt_hash", hash)
	return fmt.Errorf("duplicate of task %s (same prompt within 24h; use --force to submit anyway): %w", dup.ID[:8], store.ErrConflict)
}

// promptHashOf is the intake dedupe key: sha256 hex of the trimmed prompt.
func promptHashOf(prompt string) string {
	sum := sha256.Sum256([]byte(strings.TrimSpace(prompt)))
	return hex.EncodeToString(sum[:])
}

// steerRequest is POST /api/v1/attempts/{id}/steer: one operator turn
// injected into a running agent's session (M11 steer, DESIGN §22).
type steerRequest struct {
	Text string `json:"text"`
}

// steerAttempt queues the turn on the daemon; the worker's next heartbeat
// (≤10 s) carries it to the attempt, which writes it to the live process's
// stdin as a stream-json user message. The route is registered in
// verifyRoutes beside the other M4+ target/attempt decisions.
func (s *Server) steerAttempt(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	var req steerRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	text := strings.TrimSpace(req.Text)
	if text == "" {
		return 0, nil, badRequest("text is required")
	}
	if len(text) > protocol.MaxPromptBytes {
		return 0, nil, badRequest("text exceeds %d bytes", protocol.MaxPromptBytes)
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		a, err := tx.GetAttempt(ctx, id)
		if err != nil {
			return err
		}
		t, err := tx.GetTarget(ctx, a.TargetID)
		if err != nil {
			return err
		}
		if t.State != model.Running {
			return fmt.Errorf("target %s is %s, not running: %w", t.ID, t.State, store.ErrConflict)
		}
		return tx.EnqueueSteer(ctx, id, text)
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "steer queued", "attempt_id", id, "bytes", len(text))
	return http.StatusAccepted, map[string]any{"queued": true}, nil
}

// titleFromPrompt is the prompt's first line, bounded, or the routine's name.
func titleFromPrompt(prompt, fallback string) string {
	line, _, _ := strings.Cut(strings.TrimSpace(prompt), "\n")
	line = strings.TrimSpace(line)
	if line == "" {
		return fallback
	}
	if runes := []rune(line); len(runes) > titleMaxRunes {
		return string(runes[:titleMaxRunes])
	}
	return line
}

// workSummary is one row of GET /api/v1/work.
type workSummary struct {
	Work    store.Work      `json:"work"`
	State   model.WorkState `json:"state"`
	Targets []store.Target  `json:"targets"`
}

func (s *Server) listWork(r *http.Request) (int, any, error) {
	ctx := r.Context()
	limit, err := listLimit(r)
	if err != nil {
		return 0, nil, err
	}
	work, err := s.store.ListWork(ctx, limit)
	if err != nil {
		return 0, nil, err
	}
	ids := make([]string, len(work))
	for i, wk := range work {
		ids[i] = wk.ID
	}
	targets, err := s.store.TargetsForWorks(ctx, ids)
	if err != nil {
		return 0, nil, err
	}
	out := make([]workSummary, 0, len(work))
	for _, wk := range work {
		ts := targets[wk.ID]
		if ts == nil {
			ts = []store.Target{}
		}
		out = append(out, workSummary{Work: wk, State: model.DeriveWorkState(model.WorkInputs{Targets: engine.TargetStates(ts), Integrate: wk.Integrate}), Targets: ts})
	}
	return http.StatusOK, out, nil
}

// workDetail is GET /api/v1/work/{id}: the Work, its derived state (without
// dependency or budget effects — the queue shows those), Targets, attempts, and
// questions.
type workDetail struct {
	Work      store.Work       `json:"work"`
	State     model.WorkState  `json:"state"`
	Targets   []store.Target   `json:"targets"`
	Attempts  []store.Attempt  `json:"attempts"`
	Questions []store.Question `json:"questions"`
}

func (s *Server) workDetail(ctx context.Context, id string) (int, any, error) {
	wk, err := s.store.GetWork(ctx, id)
	if err != nil {
		return 0, nil, err
	}
	ts, err := s.store.TargetsForWork(ctx, id)
	if err != nil {
		return 0, nil, err
	}
	ids := make([]string, len(ts))
	for i, t := range ts {
		ids[i] = t.ID
	}
	byTarget, err := s.store.AttemptsForTargets(ctx, ids)
	if err != nil {
		return 0, nil, err
	}
	attempts := []store.Attempt{}
	for _, t := range ts {
		attempts = append(attempts, byTarget[t.ID]...)
	}
	// Attach the live progress tally to each unfinished attempt so the task view
	// (CLI and UI) can render running turns/tokens, phase, and the latest note.
	for i := range attempts {
		if !attempts[i].FinishedAt.IsZero() {
			continue
		}
		p, err := s.store.AttemptProgress(ctx, attempts[i].ID)
		if err != nil {
			return 0, nil, err
		}
		attempts[i].Progress = p
	}
	qs, err := s.store.QuestionsForWork(ctx, id)
	if err != nil {
		return 0, nil, err
	}
	if ts == nil {
		ts = []store.Target{}
	}
	if qs == nil {
		qs = []store.Question{}
	}
	state := model.DeriveWorkState(model.WorkInputs{Targets: engine.TargetStates(ts), Integrate: wk.Integrate})
	return http.StatusOK, workDetail{Work: *wk, State: state, Targets: ts, Attempts: attempts, Questions: qs}, nil
}

func (s *Server) getWork(r *http.Request) (int, any, error) {
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	return s.workDetail(r.Context(), id)
}

// cancelWork is exempt from the drain refusal: cancelling hastens a drain.
func (s *Server) cancelWork(r *http.Request) (int, any, error) {
	ctx := r.Context()
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return tx.CancelWork(ctx, id, "human") }); err != nil {
		return 0, nil, err
	}
	if w, werr := s.store.GetWork(ctx, id); werr == nil && w.WorkflowRunID != "" {
		s.KickFlow(ctx, w.WorkflowRunID)
	}
	s.log.InfoContext(ctx, "work cancelled", "work_id", id)
	return s.workDetail(ctx, id)
}

// workPatch is PATCH /api/v1/work/{id}: the queue's reorder and dependency edits.
type workPatch struct {
	Priority        *int         `json:"priority"`
	AddBlockedBy    []dependency `json:"add_blocked_by"`
	RemoveBlockedBy []string     `json:"remove_blocked_by"`
	// MoveBefore reorders: place this Work immediately above the named one ("" =
	// the queue's tail). The daemon refuses an order that would put a Work above
	// one it is blocked by — the one rule for the CLI and the UI drag alike.
	MoveBefore *string `json:"move_before"`
}

type dependency struct {
	WorkID string             `json:"work_id"`
	On     model.DependencyOn `json:"on"`
	// StackOn marks a stacking edge (DESIGN.md §20): the dependant may start
	// on this dependency's branch head before it merges.
	StackOn bool `json:"stack_on"`
}

func (s *Server) patchWork(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	var patch workPatch
	if err := decodeJSON(r, &patch); err != nil {
		return 0, nil, err
	}
	for i, d := range patch.AddBlockedBy {
		if err := model.ValidateID(d.WorkID); err != nil {
			return 0, nil, badRequest("add_blocked_by: %v", err)
		}
		switch d.On {
		case "":
			patch.AddBlockedBy[i].On = model.OnSuccess
		case model.OnSuccess, model.OnTerminal:
		default:
			return 0, nil, badRequest("add_blocked_by: on %q: want success or terminal", d.On)
		}
	}
	for _, dep := range patch.RemoveBlockedBy {
		if err := model.ValidateID(dep); err != nil {
			return 0, nil, badRequest("remove_blocked_by: %v", err)
		}
	}
	if patch.MoveBefore != nil {
		status, body, err := s.moveWork(ctx, id, *patch.MoveBefore)
		if err != nil || patch.Priority == nil && len(patch.AddBlockedBy) == 0 && len(patch.RemoveBlockedBy) == 0 {
			return status, body, err
		}
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		if patch.Priority != nil {
			if err := tx.SetPriority(ctx, id, *patch.Priority); err != nil {
				return err
			}
		}
		for _, d := range patch.AddBlockedBy {
			if _, err := tx.GetWork(ctx, d.WorkID); err != nil {
				return err
			}
			if err := tx.AddDependency(ctx, model.Edge{Work: id, BlockedBy: d.WorkID, On: d.On, StackOn: d.StackOn}); err != nil {
				return err
			}
		}
		for _, dep := range patch.RemoveBlockedBy {
			if err := tx.RemoveDependency(ctx, id, dep); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "work patched", "work_id", id, "priority", patch.Priority != nil, "added", len(patch.AddBlockedBy), "removed", len(patch.RemoveBlockedBy))
	return s.workDetail(ctx, id)
}

// moveWork places one Work immediately above another (or at the tail),
// refusing an order that puts it above a dependency (queue.Violates — the same
// rule the drag UI relies on).
func (s *Server) moveWork(ctx context.Context, id, before string) (int, any, error) {
	if before != "" {
		if err := model.ValidateID(before); err != nil {
			return 0, nil, badRequest("move_before: %v", err)
		}
	}
	order, err := s.loadQueue(ctx)
	if err != nil {
		return 0, nil, err
	}
	inQueue := false
	for _, e := range order {
		if e.Work.ID == id {
			inQueue = true
		}
	}
	if !inQueue {
		return 0, nil, fmt.Errorf("work %s: %w", id, store.ErrNotFound)
	}
	edges, err := s.store.DependencyEdges(ctx)
	if err != nil {
		return 0, nil, err
	}
	if before != "" && engine.Violates(order, edges, id, before) {
		return 0, nil, fmt.Errorf("moving %s above %s would put it before a task it is blocked by: %w", id[:8], before[:8], store.ErrConflict)
	}
	priority := 0
	found := false
	if before == "" {
		for _, e := range order {
			if e.Work.ID == id {
				found = true
			}
			if e.Work.Priority-1 < priority || priority == 0 {
				priority = e.Work.Priority - 1
			}
		}
	} else {
		for _, e := range order {
			if e.Work.ID == id {
				found = true
			}
			if e.Work.ID == before {
				priority = e.Work.Priority + 1
			}
		}
	}
	if !found {
		return 0, nil, fmt.Errorf("work %s: %w", id, store.ErrNotFound)
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error { return tx.SetPriority(ctx, id, priority) })
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "work moved", "work_id", id, "before", before, "priority", priority)
	return s.workDetail(ctx, id)
}

// queueItem is one row of GET /api/v1/queue.
type queueItem struct {
	Work     store.Work      `json:"work"`
	State    model.WorkState `json:"state"`
	Reason   string          `json:"reason,omitempty"`
	Waiting  []string        `json:"waiting,omitempty"`
	FailedOn []string        `json:"failed_on,omitempty"`
	Position int             `json:"position"`
	Targets  []store.Target  `json:"targets"`
}

// loadQueue is the read-only twin of claimTx's view, for display.
func (s *Server) loadQueue(ctx context.Context) ([]engine.QueueEntry, error) {
	work, err := s.store.OpenWork(ctx)
	if err != nil {
		return nil, err
	}
	ids := make([]string, len(work))
	for i, wk := range work {
		ids[i] = wk.ID
	}
	targets, err := s.store.TargetsForWorks(ctx, ids)
	if err != nil {
		return nil, err
	}
	edges, err := s.store.DependencyEdges(ctx)
	if err != nil {
		return nil, err
	}
	finished, err := s.finishedStates(ctx, work, edges)
	if err != nil {
		return nil, err
	}
	leases, err := s.store.PathLeasesRead(ctx)
	if err != nil {
		return nil, err
	}
	return engine.Order(engine.QueueInput{Work: work, Targets: targets, Edges: edges, Deferred: s.deferred, FinishedStates: finished, Leases: toPathLeases(leases), LeaseExempt: s.leaseExempt}), nil
}

func (s *Server) queue(r *http.Request) (int, any, error) {
	order, err := s.loadQueue(r.Context())
	if err != nil {
		return 0, nil, err
	}
	out := make([]queueItem, 0, len(order))
	for _, e := range order {
		ts := e.Targets
		if ts == nil {
			ts = []store.Target{}
		}
		out = append(out, queueItem{Work: e.Work, State: e.State, Reason: e.Reason, Waiting: e.Waiting, FailedOn: e.FailedOn, Position: e.Position, Targets: ts})
	}
	return http.StatusOK, out, nil
}

// answerRequest is POST /api/v1/questions/{id}/answer.
type answerRequest struct {
	Answer string `json:"answer"`
	By     string `json:"by"`
}

func (s *Server) answer(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	var req answerRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	if strings.TrimSpace(req.Answer) == "" {
		return 0, nil, badRequest("answer is required")
	}
	if req.By == "" {
		req.By = "human"
	}
	var q *store.Question
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		q, err = tx.AnswerQuestion(ctx, id, req.Answer, req.By)
		return err
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "question answered", "question_id", q.ID, "attempt_id", q.AttemptID, "target_id", q.TargetID, "by", req.By)
	return http.StatusOK, q, nil
}

// attemptDetail is GET /api/v1/attempts/{id}; facts is null until the attempt
// is terminal.
type attemptDetail struct {
	Attempt store.Attempt       `json:"attempt"`
	Facts   *store.AttemptFacts `json:"facts"`
}

func (s *Server) getAttempt(r *http.Request) (int, any, error) {
	ctx := r.Context()
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	a, err := s.store.GetAttempt(ctx, id)
	if err != nil {
		return 0, nil, err
	}
	facts, err := s.store.FactsForAttempt(ctx, id)
	if err != nil && !errors.Is(err, store.ErrNotFound) {
		return 0, nil, err
	}
	return http.StatusOK, attemptDetail{Attempt: *a, Facts: facts}, nil
}

func (s *Server) getEvents(r *http.Request) (int, any, error) {
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	limit, err := listLimit(r)
	if err != nil {
		return 0, nil, err
	}
	lines := r.URL.Query().Get("lines") != "false"
	events, err := s.store.Events(r.Context(), id, lines, limit)
	if err != nil {
		return 0, nil, err
	}
	if events == nil {
		events = []store.StoredEvent{}
	}
	return http.StatusOK, events, nil
}

func (s *Server) workers(r *http.Request) (int, any, error) {
	workers, err := s.store.Workers(r.Context(), s.now().UTC())
	if err != nil {
		return 0, nil, err
	}
	if workers == nil {
		workers = []store.Worker{}
	}
	return http.StatusOK, workers, nil
}

func (s *Server) repositories(r *http.Request) (int, any, error) {
	ctx := r.Context()
	repos, err := s.store.Repositories(ctx)
	if err != nil {
		return 0, nil, err
	}
	states, err := RepositoryStates(ctx, s.store, s.now())
	if err != nil {
		return 0, nil, err
	}
	out := make([]repoSummary, 0, len(repos))
	for _, repo := range repos {
		out = append(out, repoSummary{Repository: repo, State: states[repo.Name]})
	}
	return http.StatusOK, out, nil
}

// attention is GET /api/v1/attention: what needs a human — open questions and
// undecided proposals (M3 adds conflicts and L3 approvals).
type attention struct {
	Questions []store.Question `json:"questions"`
	Proposals []store.Proposal `json:"proposals"`
}

func (s *Server) attention(r *http.Request) (int, any, error) {
	qs, err := s.store.OpenQuestions(r.Context())
	if err != nil {
		return 0, nil, err
	}
	if qs == nil {
		qs = []store.Question{}
	}
	ps, err := s.store.ListProposals(r.Context(), model.ProposalProposed)
	if err != nil {
		return 0, nil, err
	}
	if ps == nil {
		ps = []store.Proposal{}
	}
	return http.StatusOK, attention{Questions: qs, Proposals: ps}, nil
}

// retryRequest is POST /api/v1/targets/{id}/retry's optional body. Model is
// M11's model-override knob; the schema cannot record a per-target override
// yet, so a non-empty value is refused (store.ErrModelOverride → 400).
type retryRequest struct {
	Model string `json:"model"`
}

// retryTarget is M11's "forge task retry": a failed, unverified, or cancelled
// Target goes back to pending for a fresh attempt. The state rule lives in
// model.Transition; an ineligible state maps to 409 like the other target
// decisions. Registered in verifyRoutes beside approve and reject.
func (s *Server) retryTarget(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	var req retryRequest
	if r.ContentLength != 0 {
		if err := decodeJSON(r, &req); err != nil {
			return 0, nil, err
		}
	}
	var target *store.Target
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		target, err = tx.RetryTarget(ctx, id, req.Model)
		return err
	})
	if errors.Is(err, store.ErrModelOverride) {
		return 0, nil, badRequest("%v", err)
	}
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "target retried", "target_id", id)
	return http.StatusOK, target, nil
}

// requeueTarget resolves a conflict by hand (DESIGN.md §4.1: `forge task
// requeue` after the human fixed the retained worktree): conflict →
// queued_for_merge. The transition table refuses every other state (409).
func (s *Server) requeueTarget(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	var target *store.Target
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		target, err = tx.Transition(ctx, id, model.QueuedForMerge, store.TransitionOptions{Actor: "human"})
		if err != nil {
			return err
		}
		// A human requeue means the blocker is fixed; the exhausted rebase
		// counter must not outlive it, or this transition round-trips
		// straight back to conflict.
		return tx.ResetMergeAttempts(ctx, id)
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "target requeued for merge", "target_id", id)
	return http.StatusOK, target, nil
}
