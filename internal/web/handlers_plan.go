package web

import (
	"context"
	"encoding/json"
	"fmt"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

// Plan-batch bounds and per-task defaults. A planned task is real coding
// work, so its budget is the implement-shaped default, not the small verify
// one; the plan's own snapshot supplies executor and model so tests (and the
// fake executor) flow through unchanged.
const (
	planMaxTasks     = 20
	planTaskMode     = "run"
	planTaskTimeout  = 3600
	planTaskMaxTurns = 50
)

// planTask is one entry of a plan (or supervise-revise) result's "tasks"
// array (schema.PlanTasks). BlockedBy entries are indexes into the same
// array; Mode "plan" nests decomposition, depth-capped by createTaskBatch.
type planTask struct {
	Title     string   `json:"title"`
	Prompt    string   `json:"prompt"`
	Paths     []string `json:"paths"`
	BlockedBy []int    `json:"blocked_by"`
	StackOn   bool     `json:"stack_on"`
	Size      string   `json:"size"`
	Mode      string   `json:"mode"`
	Tier      *int     `json:"tier"`
}

// planFollowUps creates the batch a successful plan attempt described
// (DESIGN.md §20): one Work per task on the plan's repository, with the
// plan's autonomy, class, and integrate flag, the task's write-set globs,
// blocked_by edges (with stack_on hints on the edge), and the plan Work's id
// as plan_batch_id so the batch is visible as one. It runs inside the
// completion transaction: the batch appears together or not at all. Planned
// tasks run in `run` mode — L1, the level the merge queue re-checks — so a
// batch does not multiply into L2 verify chains.
func (s *Engine) planFollowUps(ctx context.Context, tx *store.Tx, a *store.Attempt, w *store.Work, t *store.Target, env *protocol.ResultEnvelope) error {
	tasks, err := planTasks(env)
	if err != nil {
		return fmt.Errorf("plan %s: %w", a.ID, err)
	}
	if len(tasks) == 0 {
		return nil
	}
	// Idempotence: a reopened plan target that completes again must not
	// duplicate its batch.
	if has, err := tx.HasBatch(ctx, w.ID); err != nil {
		return err
	} else if has {
		return tx.Journal(ctx, "plan.batch_exists", store.EntityWork, w.ID, map[string]any{"attempt_id": a.ID})
	}
	snap, err := snapshotRoutine(*w)
	if err != nil {
		return err
	}
	// integrate travels in the snapshot: the plan Work's own flag is cleared
	// at creation (a plan has nothing to merge), the hint survives here.
	integrate := w.Integrate || snap.Integrate
	ids, titles, err := s.createTaskBatch(ctx, tx, w, t.Repository, tasks, snap, integrate)
	if err != nil {
		return err
	}
	if err := tx.Journal(ctx, "plan.batch_created", store.EntityWork, w.ID, map[string]any{"tasks": ids, "repository": t.Repository, "attempt_id": a.ID}); err != nil {
		return err
	}
	s.log.InfoContext(ctx, "plan batch created", "work_id", w.ID, "tasks", len(ids), "repository", t.Repository)
	if supervise, _, _ := s.planLimits(); supervise {
		goal := snap.Objective
		if goal == "" {
			goal = snap.Prompt
		}
		if _, err := s.createContinuation(ctx, tx, w, t, snap, ids, titles, 1, goal, env.Summary, integrate); err != nil {
			return err
		}
	}
	return nil
}

// createTaskBatch creates one Work per task on repo, caused by creator, with
// intra-batch edges; PlanBatchID = creator.ID. Shared by plan and by
// supervise's revise rounds. A task asking for mode "plan" nests
// decomposition, demoted to run past the [plan] max_nesting depth.
func (s *Engine) createTaskBatch(ctx context.Context, tx *store.Tx, creator *store.Work, repo string, tasks []planTask, snap store.Routine, integrate bool) (ids, titles []string, err error) {
	_, _, maxNesting := s.planLimits()
	depth, err := s.planDepth(ctx, tx, creator)
	if err != nil {
		return nil, nil, err
	}
	ids, titles = make([]string, len(tasks)), make([]string, len(tasks))
	for i, task := range tasks {
		rt := store.Routine{
			Name: "plan-task", Mode: planTaskMode, Prompt: task.Prompt, Repositories: []string{repo},
			Executor: snap.Executor, Model: snap.Model, MaxTurns: planTaskMaxTurns, TimeoutSeconds: planTaskTimeout,
			Autonomy: creator.Autonomy, Priority: creator.Priority, BudgetClass: creator.BudgetClass, Concurrency: 1,
			Paths: task.Paths, Integrate: integrate, MaxQuestions: adHocMaxQuestions, RequireSandbox: snap.RequireSandbox,
		}
		workIntegrate := integrate
		if task.Mode == "plan" {
			if depth >= maxNesting {
				// One over-eager task must not fail the whole completion tx:
				// it still gets done, just not decomposed.
				if err := tx.Journal(ctx, "plan.nesting_capped", store.EntityWork, creator.ID, map[string]any{"task": task.Title, "depth": depth}); err != nil {
					return nil, nil, err
				}
			} else {
				rt.Mode = "plan"
				// A plan writes nothing to merge: the Work flag stays off and
				// the hint travels in the snapshot (the handlers_operator.go
				// modeWritesNothing trick, done by hand here).
				workIntegrate = false
				if depth+1 >= maxNesting {
					rt.Prompt += "\n\nYou may not emit plan-mode tasks (the nesting limit is reached): every task must be directly executable."
				}
			}
		}
		blob, err := json.Marshal(rt)
		if err != nil {
			return nil, nil, fmt.Errorf("snapshot plan task %d: %w", i, err)
		}
		title := task.Title
		if title == "" {
			title = titleFromPrompt(task.Prompt, "plan-task")
		}
		size := task.Size
		if !store.ValidSize(size) {
			size = ""
		}
		work := &store.Work{
			RoutineName: "plan-task", Title: title, Trigger: model.TriggerDependency, Snapshot: blob,
			Priority: creator.Priority, BudgetClass: creator.BudgetClass, Autonomy: creator.Autonomy, Integrate: workIntegrate,
			Paths: task.Paths, Tier: task.Tier, Size: size, PlanBatchID: creator.ID, PromptHash: promptHashOf(rt.Prompt),
			SubmittedBy:    "plan:" + model.ShortID(creator.ID),
			CausedByWorkID: creator.ID, Cause: model.CausePlanTask,
		}
		if _, err := tx.CreateWork(ctx, work, []string{repo}, nil); err != nil {
			return nil, nil, fmt.Errorf("create plan task %d: %w", i, err)
		}
		ids[i], titles[i] = work.ID, title
	}
	// Edges second, once every id exists — blocked_by may point forward.
	for i, task := range tasks {
		for _, dep := range task.BlockedBy {
			if err := tx.AddDependency(ctx, model.Edge{Work: ids[i], BlockedBy: ids[dep], On: model.OnSuccess, StackOn: task.StackOn}); err != nil {
				return nil, nil, fmt.Errorf("edge for plan task %d: %w", i, err)
			}
		}
	}
	return ids, titles, nil
}

// planTasks decodes and validates the tasks array. A malformed plan is an
// error — the schema enforced the shape, so this is belt and braces.
func planTasks(env *protocol.ResultEnvelope) ([]planTask, error) {
	if env == nil || env.Extra == nil {
		return nil, nil
	}
	raw, ok := env.Extra["tasks"]
	if !ok {
		return nil, nil
	}
	var tasks []planTask
	if err := json.Unmarshal(raw, &tasks); err != nil {
		return nil, fmt.Errorf("decode tasks: %w", err)
	}
	if len(tasks) > planMaxTasks {
		return nil, fmt.Errorf("%d tasks exceed %d", len(tasks), planMaxTasks)
	}
	for i, task := range tasks {
		if task.Prompt == "" {
			return nil, fmt.Errorf("task %d has no prompt", i)
		}
		for _, dep := range task.BlockedBy {
			if dep < 0 || dep >= len(tasks) {
				return nil, fmt.Errorf("task %d: blocked_by %d out of range", i, dep)
			}
			if dep == i {
				return nil, fmt.Errorf("task %d blocks on itself", i)
			}
		}
	}
	return tasks, nil
}
