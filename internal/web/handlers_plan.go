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

// planTask is one entry of a plan result's "tasks" array (modes/plan).
// BlockedBy entries are indexes into the same array.
type planTask struct {
	Title     string   `json:"title"`
	Prompt    string   `json:"prompt"`
	Paths     []string `json:"paths"`
	BlockedBy []int    `json:"blocked_by"`
	StackOn   bool     `json:"stack_on"`
	Size      string   `json:"size"`
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
	snap, err := snapshotRoutine(*w)
	if err != nil {
		return err
	}
	// integrate travels in the snapshot: the plan Work's own flag is cleared
	// at creation (a plan has nothing to merge), the hint survives here.
	integrate := w.Integrate || snap.Integrate
	ids := make([]string, len(tasks))
	for i, task := range tasks {
		rt := store.Routine{
			Name: "plan-task", Mode: planTaskMode, Prompt: task.Prompt, Repositories: []string{t.Repository},
			Executor: snap.Executor, Model: snap.Model, MaxTurns: planTaskMaxTurns, TimeoutSeconds: planTaskTimeout,
			Autonomy: w.Autonomy, Priority: w.Priority, BudgetClass: w.BudgetClass, Concurrency: 1,
			Paths: task.Paths, Integrate: integrate, MaxQuestions: adHocMaxQuestions, RequireSandbox: snap.RequireSandbox,
		}
		blob, err := json.Marshal(rt)
		if err != nil {
			return fmt.Errorf("snapshot plan task %d: %w", i, err)
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
			Priority: w.Priority, BudgetClass: w.BudgetClass, Autonomy: w.Autonomy, Integrate: integrate,
			Paths: task.Paths, Tier: task.Tier, Size: size, PlanBatchID: w.ID, PromptHash: promptHashOf(task.Prompt),
			SubmittedBy:    "plan:" + model.ShortID(w.ID),
			CausedByWorkID: w.ID, Cause: model.CausePlanTask,
		}
		if _, err := tx.CreateWork(ctx, work, []string{t.Repository}, nil); err != nil {
			return fmt.Errorf("create plan task %d: %w", i, err)
		}
		ids[i] = work.ID
	}
	// Edges second, once every id exists — blocked_by may point forward.
	for i, task := range tasks {
		for _, dep := range task.BlockedBy {
			if err := tx.AddDependency(ctx, model.Edge{Work: ids[i], BlockedBy: ids[dep], On: model.OnSuccess, StackOn: task.StackOn}); err != nil {
				return fmt.Errorf("edge for plan task %d: %w", i, err)
			}
		}
	}
	if err := tx.Journal(ctx, "plan.batch_created", store.EntityWork, w.ID, map[string]any{"tasks": ids, "repository": t.Repository, "attempt_id": a.ID}); err != nil {
		return err
	}
	s.log.InfoContext(ctx, "plan batch created", "work_id", w.ID, "tasks", len(ids), "repository", t.Repository)
	return nil
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
