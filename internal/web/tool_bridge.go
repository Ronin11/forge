package web

// The daemon side of the tool/skill bridge: the closures forge_directive_run
// and forge_workflow_run reach through tools.Deps. This is where the
// guardrails live — since an empty AllowedTools list exposes every tool to
// every agent, safety is enforced at the call, not the listing: tool-flag
// checks happen in the tools, and here the spawn depth cap, the per-work
// spawn cap, the class ceiling, and the provenance stamps
// (cause=tool, submitted_by=agent:<attempt>, caused_by=the caller's work).

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"forge/internal/core/directives"
	"forge/internal/core/engine"
	"forge/internal/core/flow"
	"forge/internal/core/model"
	"forge/internal/core/store"
	"forge/internal/tools"
)

const (
	// maxToolSpawnDepth: a spawned Work's agent may not spawn further —
	// depth counts consecutive cause=tool links in the caused_by chain.
	maxToolSpawnDepth = 1
	// maxToolSpawnsPerWork bounds fan-out from one task.
	maxToolSpawnsPerWork = 5
	// spawnAncestryBound stops a corrupt caused_by chain from looping.
	spawnAncestryBound = 10
)

// spawnWorkForTool is Deps.SpawnWork: guardrails, then the ordinary
// directive materialization path of createWorkTx.
func (s *Server) spawnWorkForTool(ctx context.Context, att tools.Attempt, in tools.SpawnInput) (string, error) {
	if in.Class == "" {
		in.Class = model.ClassBacklog
	}
	if in.Class == model.ClassInteractive {
		return "", tools.BadInput("class interactive is reserved for humans")
	}
	var out workCreated
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		// Depth cap: walk the caused_by chain counting tool links.
		depth := 0
		workID := att.WorkID
		for hop := 0; hop < spawnAncestryBound && workID != ""; hop++ {
			w, err := tx.GetWork(ctx, workID)
			if err != nil {
				return err
			}
			if w.Cause != model.CauseTool {
				break
			}
			depth++
			workID = w.CausedByWorkID
		}
		if depth >= maxToolSpawnDepth {
			return tools.BadInput("spawned work may not spawn further (depth %d): finish the task yourself or ask a human", depth)
		}
		// Fan-out cap per calling work.
		n, err := tx.CountToolSpawns(ctx, att.WorkID)
		if err != nil {
			return err
		}
		if n >= maxToolSpawnsPerWork {
			return tools.BadInput("this task already spawned %d sub-tasks (the cap): work with what you have", n)
		}
		created, err := s.createWorkTx(ctx, tx, workRequest{
			directive:    in.Directive,
			Objective:    in.Objective,
			Repositories: in.Repositories,
			Class:        in.Class,
			CausedBy:     att.WorkID,
			// The child never runs more autonomously than its parent.
			Autonomy:    att.Autonomy,
			cause:       model.CauseTool,
			submittedBy: "agent:" + att.ID,
		})
		if err != nil {
			return err
		}
		out = created
		return tx.Journal(ctx, "tool.spawned_work", store.EntityWork, out.Work.ID, map[string]any{
			"attempt": att.ID, "parent_work": att.WorkID, "directive": in.Directive, "class": in.Class,
		})
	})
	if err != nil {
		return "", err
	}
	s.log.InfoContext(ctx, "agent spawned work", "attempt", att.ID, "parent_work", att.WorkID, "work_id", out.Work.ID, "directive", in.Directive)
	return out.Work.ID, nil
}

// startWorkflowRunForTool is Deps.StartWorkflowRun: the runWorkflow shape
// with tool provenance journaled. The tool already checked the flag; the row
// is re-read inside the tx so a race with archive/un-flag still refuses.
func (s *Server) startWorkflowRunForTool(ctx context.Context, att tools.Attempt, workflow, objective string, repos []string) (string, error) {
	run := &store.WorkflowRun{Trigger: model.TriggerManual, Context: store.RunContext{Repositories: repos, Objective: objective}}
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		wf, err := tx.GetWorkflow(ctx, workflow)
		if err != nil {
			return err
		}
		if !wf.ArchivedAt.IsZero() || !wf.Tool {
			return tools.BadInput("workflow %q is not callable", workflow)
		}
		run.WorkflowID, run.WorkflowName, run.WorkflowGeneration, run.Graph = wf.ID, wf.Name, wf.Generation, wf.Graph
		if err := tx.CreateWorkflowRun(ctx, run); err != nil {
			return err
		}
		return tx.Journal(ctx, "tool.workflow_run", store.EntityWorkflow, run.ID, map[string]any{
			"attempt": att.ID, "parent_work": att.WorkID, "workflow": workflow,
		})
	})
	if err != nil {
		return "", err
	}
	s.advanceRun(ctx, run.ID)
	s.log.InfoContext(ctx, "agent fired workflow run", "attempt", att.ID, "workflow", workflow, "run_id", run.ID)
	return run.ID, nil
}

// scratchDefaults fills zero config (bare test servers).
func (s *Server) scratchLimits() (max, promoteRuns, promoteAttempts int) {
	max, promoteRuns, promoteAttempts = s.scratchCfg.Max, s.scratchCfg.PromoteRuns, s.scratchCfg.PromoteAttempts
	if max <= 0 {
		max = 200
	}
	if promoteRuns <= 0 {
		promoteRuns = 5
	}
	if promoteAttempts <= 0 {
		promoteAttempts = 2
	}
	return max, promoteRuns, promoteAttempts
}

// scratchForTool is Deps.Scratch: save (optional), run, count, and — past
// the threshold — promote into the git library. The organic layer: agents
// reach for quick scripts through here; what keeps getting reached for
// stops being ephemeral.
func (s *Server) scratchForTool(ctx context.Context, att tools.Attempt, in tools.ScratchInput) (json.RawMessage, tools.ScratchMeta, error) {
	var meta tools.ScratchMeta
	lib := s.promptLibrary()
	if lib != nil && lib.Fragment(in.Name) != nil {
		return nil, meta, tools.BadInput("%q is a library name — run it via forge_script_run (or pick another scratch name)", in.Name)
	}
	max, promoteRuns, promoteAttempts := s.scratchLimits()

	// Save (or replace) when source is given; always load + touch.
	var row *store.ScratchScript
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		if in.Source != "" {
			sc := &store.ScratchScript{Name: in.Name, Source: in.Source, Language: in.Language,
				Description: in.Description, InputSchema: in.InputSchema, CreatedBy: "agent:" + att.ID}
			if att.ID == "" {
				sc.CreatedBy = "human"
			}
			if err := tx.UpsertScratch(ctx, sc, max); err != nil {
				return err
			}
			if err := tx.Journal(ctx, "scratch.saved", store.EntityDaemon, in.Name, map[string]any{"attempt": att.ID, "language": in.Language, "hash": sc.Hash}); err != nil {
				return err
			}
		}
		var err error
		row, err = tx.TouchScratchRun(ctx, in.Name, att.ID)
		return err
	})
	if err != nil {
		return nil, meta, err
	}

	// Execute: js in the goja sandbox, anything else as a subprocess from
	// the materialized cache dir.
	input := flow.ScriptInput{Params: in.Input}
	var out json.RawMessage
	if row.Language == "js" {
		out, err = flow.RunScript(row.Source, input, flow.ScriptTimeout(0))
	} else {
		dir := filepath.Join(s.home, "scratch-scripts")
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return nil, meta, err
		}
		path := filepath.Join(dir, row.Name+"."+row.Language)
		if err := os.WriteFile(path, []byte(row.Source), 0o644); err != nil {
			return nil, meta, err
		}
		var interp []string
		interp, err = directives.ResolveInterpreter(row.Source, path)
		if err != nil {
			return nil, meta, tools.BadInput("%v", err)
		}
		out, err = flow.RunExternal(interp, path, input, flow.ExternalTimeout(0))
	}
	if err != nil {
		return nil, meta, tools.BadInput("scratch %s: %v", row.Name, err)
	}
	meta.RunCount = row.RunCount

	// Promotion threshold: enough runs across enough distinct attempts →
	// queue a high-priority curation Work (directive promote-scratch, run
	// against the library repository itself) rather than promoting inline.
	// The curator dedupes, extends, or adds — and the reconcile sweep
	// retires the cache row once the Work lands.
	if row.RunCount >= promoteRuns && len(row.Attempts) >= promoteAttempts && row.PromoteWork == "" {
		workID, err := s.queuePromotion(ctx, att, row)
		if err != nil {
			s.log.WarnContext(ctx, "queue scratch promotion", "script", row.Name, "error", err)
		} else {
			meta.PromotionWork = workID
		}
	}
	return out, meta, nil
}

// promotionPriority outranks routine work (default 50): promoted scripts are
// hot paths, so the curation task jumps the queue without being interactive.
const promotionPriority = 80

// Bounds for forge_work_outcomes: enough for any legal batch tree, small
// enough that the response stays a prompt-sized object.
const (
	outcomesMaxWorks   = 200
	outcomesSummaryCap = 500
)

// workOutcomeRow is one work's settled reality as forge_work_outcomes
// reports it.
type workOutcomeRow struct {
	ID          string   `json:"id"`
	Title       string   `json:"title"`
	Mode        string   `json:"mode"`
	Cause       string   `json:"cause,omitempty"`
	State       string   `json:"state"`
	Depth       int      `json:"depth"`
	BlockedBy   []string `json:"blocked_by,omitempty"`
	Summary     string   `json:"summary,omitempty"`
	CostUSD     float64  `json:"cost_usd,omitempty"`
	NumTurns    int      `json:"num_turns,omitempty"`
	Attempts    int      `json:"attempts"`
	PlanBatchID string   `json:"plan_batch_id,omitempty"`
	Size        string   `json:"size,omitempty"`
}

// workOutcomesForTool is Deps.WorkOutcomes: the supervise agent's view of
// what its batch actually did. Scope defaults to the caller's batch parent
// (its work's caused_by — the plan or prior continuation), so the agent sees
// its siblings' subtree; an explicit work_id must live in the caller's own
// tree.
func (s *Server) workOutcomesForTool(ctx context.Context, att tools.Attempt, workID string) (json.RawMessage, error) {
	if att.WorkID == "" {
		return nil, tools.BadInput("forge_work_outcomes needs a calling attempt")
	}
	caller, err := s.store.GetWork(ctx, att.WorkID)
	if err != nil {
		return nil, err
	}
	scope := workID
	if scope == "" {
		scope = caller.CausedByWorkID
		if scope == "" {
			scope = caller.ID
		}
	}
	lin, err := ComputeLineage(ctx, s.store, scope)
	if err != nil {
		return nil, err
	}
	callerRoot := caller.RootWorkID
	if callerRoot == "" {
		callerRoot = caller.ID
	}
	if lin.RootID != callerRoot {
		return nil, tools.BadInput("work %s is not in your tree", workID)
	}
	// Keep the subtree under scope (scope excluded only when it is the
	// caller's parent — the agent asked "what did my batch do", not "what am
	// I"), walking caused_by through the memoized ByID map.
	inScope := func(id string) bool {
		for hop := 0; hop < spawnAncestryBound*2 && id != ""; hop++ {
			if id == scope {
				return true
			}
			id = lin.ByID[id].CausedByWorkID
		}
		return false
	}
	var edgesByWork = map[string][]string{}
	for _, e := range lin.Edges {
		edgesByWork[e.Work] = append(edgesByWork[e.Work], e.BlockedBy)
	}
	rows := make([]workOutcomeRow, 0, 16)
	for _, w := range lin.Works {
		if w.ID == att.WorkID || !inScope(w.ID) {
			continue
		}
		row := workOutcomeRow{
			ID: w.ID, Title: w.Title, Cause: string(w.Cause), State: string(lin.State[w.ID]),
			Depth: lin.Depth[w.ID], BlockedBy: edgesByWork[w.ID], PlanBatchID: w.PlanBatchID, Size: w.Size,
		}
		if snap, err := snapshotRoutine(w); err == nil {
			row.Mode = snap.Mode
		}
		var targetIDs []string
		for _, t := range lin.Targets[w.ID] {
			targetIDs = append(targetIDs, t.ID)
		}
		atts, err := s.store.AttemptsForTargets(ctx, targetIDs)
		if err != nil {
			return nil, err
		}
		var latest *store.Attempt
		for _, list := range atts {
			for i := range list {
				row.Attempts++
				if latest == nil || list[i].CreatedAt.After(latest.CreatedAt) {
					latest = &list[i]
				}
			}
		}
		if latest != nil {
			if env := decodeEnvelope(latest.Result); env != nil {
				row.Summary = env.Summary
				if len(row.Summary) > outcomesSummaryCap {
					row.Summary = row.Summary[:outcomesSummaryCap] + "…"
				}
			}
			if latest.CostUSD != nil {
				row.CostUSD = *latest.CostUSD
			}
			row.NumTurns = latest.NumTurns
		}
		rows = append(rows, row)
		if len(rows) >= outcomesMaxWorks {
			break
		}
	}
	return json.Marshal(map[string]any{"scope": scope, "works": rows})
}

// queuePromotion creates the curation Work for one over-threshold scratch
// row: directive promote-scratch, run against the repository registered at
// the library's path, integrate-on-green with auto autonomy (the operator's
// fully-automatic choice — the journal, the Work trail, and git history are
// the audit). Refuses cleanly when the directive or the library repository
// is missing; the threshold re-fires on a later run.
func (s *Server) queuePromotion(ctx context.Context, att tools.Attempt, row *store.ScratchScript) (string, error) {
	lib := s.promptLibrary()
	if lib == nil {
		return "", fmt.Errorf("no library loaded")
	}
	if lib.Directive("promote-scratch") == nil {
		return "", fmt.Errorf("directive promote-scratch is not in the library")
	}
	libDir, err := filepath.EvalSymlinks(lib.Dir)
	if err != nil {
		libDir = filepath.Clean(lib.Dir)
	}
	priority := promotionPriority
	var out workCreated
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		repos, err := tx.Repositories(ctx)
		if err != nil {
			return err
		}
		repoName := ""
		for _, r := range repos {
			if r.Archived {
				continue
			}
			p, err := filepath.EvalSymlinks(r.Path)
			if err != nil {
				p = filepath.Clean(r.Path)
			}
			if p == libDir {
				repoName = r.Name
				break
			}
		}
		if repoName == "" {
			return fmt.Errorf("the library at %s is not a registered repository", lib.Dir)
		}
		req := workRequest{
			directive: "promote-scratch",
			Objective: fmt.Sprintf("Scratch script %q (%s) crossed the promotion threshold: %d runs across %d distinct agent attempts, saved by %s. Fetch it with forge_library ({\"kind\": \"scratch\", \"name\": %q}) and fold it into the library per this directive.",
				row.Name, row.Language, row.RunCount, len(row.Attempts), row.CreatedBy, row.Name),
			Repositories: []string{repoName},
			Title:        "promote scratch script " + row.Name,
			Class:        model.ClassNormal,
			Priority:     &priority,
			Autonomy:     model.AutonomyAuto,
			Integrate:    true,
			cause:        model.CausePromotion,
			submittedBy:  "forge:scratch",
		}
		if att.WorkID != "" {
			req.CausedBy = att.WorkID
		}
		created, err := s.createWorkTx(ctx, tx, req)
		if err != nil {
			return err
		}
		out = created
		if err := tx.SetScratchPromoteWork(ctx, row.Name, out.Work.ID); err != nil {
			return err
		}
		return tx.Journal(ctx, "scratch.promotion_queued", store.EntityWork, out.Work.ID, map[string]any{
			"script": row.Name, "language": row.Language, "runs": row.RunCount, "attempts": len(row.Attempts), "created_by": row.CreatedBy,
		})
	})
	if err != nil {
		return "", err
	}
	s.log.InfoContext(ctx, "scratch promotion queued", "script", row.Name, "work_id", out.Work.ID, "runs", row.RunCount)
	return out.Work.ID, nil
}

// reconcileScratch retires or retries pending promotions (called from the
// schedule tick). A row whose name now answers as a library script is done —
// the curator landed it (or it was shadowed by a manual add); a row whose
// curation Work reached a done state is also retired even when the curator
// chose a different path (extended a neighbor, metadata-only) — the cache
// entry served its purpose. A failed or cancelled Work clears promote_work
// so a later run may re-queue.
func (s *Server) reconcileScratch(ctx context.Context) {
	lib := s.promptLibrary()
	if lib == nil {
		return
	}
	rows, err := s.store.ListScratch(ctx)
	if err != nil || len(rows) == 0 {
		return
	}
	for _, row := range rows {
		if f := lib.Fragment(row.Name); f != nil && f.Script {
			s.retireScratch(ctx, row, "library")
			continue
		}
		if row.PromoteWork == "" {
			continue
		}
		var w *store.Work
		err := s.store.Write(ctx, func(tx *store.Tx) error {
			var err error
			w, err = tx.GetWork(ctx, row.PromoteWork)
			return err
		})
		var state model.WorkState
		if err == nil {
			targets, terr := s.store.TargetsForWorks(ctx, []string{w.ID})
			if terr != nil {
				err = terr
			} else {
				state = model.DeriveWorkState(model.WorkInputs{Targets: engine.TargetStates(targets[w.ID]), Integrate: w.Integrate})
			}
		}
		switch {
		case errors.Is(err, store.ErrNotFound):
			s.clearPromotion(ctx, row, "work missing")
		case err != nil:
			s.log.WarnContext(ctx, "reconcile scratch", "script", row.Name, "error", err)
		case state == model.WorkMerged || state == model.WorkSucceeded || state == model.WorkPartial:
			s.retireScratch(ctx, row, "work")
		case state == model.WorkFailed || state == model.WorkCancelled || state == model.WorkUnverified:
			s.clearPromotion(ctx, row, string(state))
		}
	}
}

// retireScratch deletes a promoted row and journals the graduation.
func (s *Server) retireScratch(ctx context.Context, row store.ScratchScript, via string) {
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		if err := tx.DeleteScratch(ctx, row.Name); err != nil {
			return err
		}
		return tx.Journal(ctx, "scratch.promoted", store.EntityDaemon, row.Name, map[string]any{
			"language": row.Language, "runs": row.RunCount, "attempts": len(row.Attempts),
			"created_by": row.CreatedBy, "via": via, "work": row.PromoteWork,
		})
	})
	if err != nil {
		s.log.WarnContext(ctx, "retire scratch", "script", row.Name, "error", err)
		return
	}
	s.log.InfoContext(ctx, "scratch script promoted", "script", row.Name, "via", via, "work_id", row.PromoteWork)
}

// clearPromotion forgets a dead curation Work so the threshold may re-queue.
func (s *Server) clearPromotion(ctx context.Context, row store.ScratchScript, reason string) {
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		if err := tx.SetScratchPromoteWork(ctx, row.Name, ""); err != nil {
			return err
		}
		return tx.Journal(ctx, "scratch.promotion_retry", store.EntityDaemon, row.Name, map[string]any{
			"work": row.PromoteWork, "reason": reason,
		})
	})
	if err != nil {
		s.log.WarnContext(ctx, "clear scratch promotion", "script", row.Name, "error", err)
	}
}

// openExperimentForTool is Deps.OpenExperiment: an agent's "uncertain →
// trial it" move. The experiment machinery is itself the guardrail —
// posterior decisions, drift aborts, guarded promotion — so agents open
// experiments directly; the one-live-per-subject index and the learning
// pool bound the blast radius.
func (s *Server) openExperimentForTool(ctx context.Context, att tools.Attempt, in tools.ExperimentInput) (string, error) {
	in.Goal = strings.TrimSpace(in.Goal)
	if in.Goal == "" {
		return "", tools.BadInput("goal is required: what should the subject do better, and how would we know")
	}
	kind, _, err := store.ParseTarget(in.Subject)
	if err != nil || (kind != store.TargetDirective && kind != "persona") {
		return "", tools.BadInput("subject %q: want directive:<name> or persona:<name>", in.Subject)
	}
	minRuns, maxArms, _, _ := s.experimentsLimits()
	if in.MinRuns > 0 {
		minRuns = in.MinRuns
	}
	if len(in.Variants) > maxArms-1 {
		return "", tools.BadInput("at most %d variants (arms include control)", maxArms-1)
	}
	if len(in.Variants) == 0 && s.modelCall == nil {
		return "", tools.BadInput("no variants given and this process has no model access to generate them")
	}
	target, optimizer := "", ""
	for _, alias := range []string{"sonnet", "haiku", "opus"} {
		if _, ok := s.resolveModel(alias); ok {
			target = alias
			break
		}
	}
	for _, alias := range []string{"fable", "opus", "sonnet"} {
		if _, ok := s.resolveModel(alias); ok {
			optimizer = alias
			break
		}
	}
	if target == "" || optimizer == "" {
		return "", fmt.Errorf("no resolvable models for an experiment")
	}
	pe := store.Experiment{
		Subject: in.Subject, Goal: in.Goal, TargetModel: target, OptimizerModel: optimizer,
		VariantCount: max(len(in.Variants), 2), Progress: "starting", Kind: store.ExperimentKindLive, MinRuns: minRuns,
	}
	subject, baseline, err := s.experimentSubjectFor(ctx, &pe)
	if err != nil {
		return "", tools.BadInput("%v", err)
	}
	pe.Baseline = baseline
	var provided []experimentCandidate
	for i, v := range in.Variants {
		if strings.TrimSpace(v.Content) == "" {
			return "", tools.BadInput("variant %d: content is required", i+1)
		}
		title := v.Title
		if title == "" {
			title = fmt.Sprintf("variant %d", i+1)
		}
		provided = append(provided, experimentCandidate{Title: title, Content: v.Content})
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error {
		if err := tx.InsertExperiment(ctx, &pe); err != nil {
			return err
		}
		return tx.Journal(ctx, "experiment.agent_opened", store.EntityDaemon, pe.ID, map[string]any{
			"subject": in.Subject, "attempt_id": att.ID, "goal": in.Goal, "provided_variants": len(provided)})
	}); err != nil {
		return "", err
	}
	go s.runLiveSetup(pe, subject, maxArms, "", provided)
	s.log.InfoContext(ctx, "agent opened live experiment", "id", pe.ID, "subject", in.Subject, "attempt_id", att.ID)
	return pe.ID, nil
}
