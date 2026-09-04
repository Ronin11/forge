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
