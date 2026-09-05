package web

import (
	"context"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// The cron scheduler. Routines and workflows have carried schedule /
// schedule_enabled / next_due_at since their tables were born; this loop is
// what finally reads them. The daemon's own tick plus SQLite are the runner
// (no embedded cron runtime): each tick backfills missing next_due_at rows,
// fires what is due, and advances next_due_at in the same transaction as the
// admission — which is the claim, since one daemon owns the database. An
// occurrence missed while the daemon was down fires once, then the schedule
// jumps to the next future occurrence — no backfill storm. A due routine or
// workflow that is still busy from its last firing is skipped, journaled, and
// rescheduled: unattended queues should not pile up.

// RunSchedule is the daemon loop; it returns when ctx is done.
func (s *Server) RunSchedule(ctx context.Context, interval time.Duration) {
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			s.scheduleTick(ctx)
		}
	}
}

// scheduleTick is one pass. Errors are logged — the next tick retries;
// nothing else handles them.
func (s *Server) scheduleTick(ctx context.Context) {
	if s.Draining() {
		return
	}
	now := s.now().UTC()
	s.scheduleBackfill(ctx, now)
	s.fireDueRoutines(ctx, now)
	s.fireDueWorkflows(ctx, now)
	s.reconcileScratch(ctx)
}

// scheduleBackfill seeds next_due_at for enabled schedules that lack one:
// first boot after the upgrade, a re-enable, or an edit.
func (s *Server) scheduleBackfill(ctx context.Context, now time.Time) {
	routines, err := s.store.ListRoutines(ctx, false)
	if err != nil {
		s.log.ErrorContext(ctx, "schedule backfill: list routines", "error", err)
		return
	}
	workflows, err := s.store.ListWorkflows(ctx, false)
	if err != nil {
		s.log.ErrorContext(ctx, "schedule backfill: list workflows", "error", err)
		return
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		for _, rt := range routines {
			if !rt.ScheduleEnabled || rt.Schedule == "" || !rt.NextDueAt.IsZero() {
				continue
			}
			next, err := store.NextOccurrence(rt.Schedule, now)
			if err != nil {
				s.log.WarnContext(ctx, "unparseable routine schedule", "routine", rt.Name, "schedule", rt.Schedule, "error", err)
				continue
			}
			if err := tx.SetNextDue(ctx, rt.Name, next); err != nil {
				return err
			}
		}
		for _, wf := range workflows {
			if !wf.ScheduleEnabled || wf.Schedule == "" || !wf.NextDueAt.IsZero() {
				continue
			}
			next, err := store.NextOccurrence(wf.Schedule, now)
			if err != nil {
				s.log.WarnContext(ctx, "unparseable workflow schedule", "workflow", wf.Name, "schedule", wf.Schedule, "error", err)
				continue
			}
			if err := tx.SetWorkflowNextDue(ctx, wf.Name, next); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		s.log.ErrorContext(ctx, "schedule backfill", "error", err)
	}
}

// fireDueRoutines admits one Work per due routine, unless the routine still
// has unfinished Work from the last firing.
func (s *Server) fireDueRoutines(ctx context.Context, now time.Time) {
	due, err := s.store.DueRoutines(ctx, now)
	if err != nil {
		s.log.ErrorContext(ctx, "list due routines", "error", err)
		return
	}
	for _, rt := range due {
		if kind, target := targetOf(&rt); kind == store.TargetWorkflow {
			s.fireDueWorkflowRoutine(ctx, rt, target, now)
			continue
		} else if kind == store.TargetScript {
			s.fireDueScriptRoutine(ctx, rt, target, now)
			continue
		} else if kind == store.TargetBench {
			s.fireDueBenchRoutine(ctx, rt, target, now)
			continue
		}
		open, err := s.store.OpenWorkCountForRoutine(ctx, rt.ID)
		if err != nil {
			s.log.ErrorContext(ctx, "count open work", "routine", rt.Name, "error", err)
			continue
		}
		next, err := store.NextOccurrence(rt.Schedule, now)
		if err != nil {
			s.log.WarnContext(ctx, "unparseable routine schedule", "routine", rt.Name, "error", err)
			continue
		}
		err = s.store.Write(ctx, func(tx *store.Tx) error {
			// Re-check under the write lock: an edit or disable between the
			// read and here wins.
			saved, err := tx.GetRoutine(ctx, rt.Name)
			if err != nil || !saved.ScheduleEnabled || !saved.ArchivedAt.IsZero() {
				return err
			}
			if err := tx.SetNextDue(ctx, rt.Name, next); err != nil {
				return err
			}
			if open > 0 {
				return tx.Journal(ctx, "schedule.skipped", store.EntityDaemon, saved.ID, map[string]any{"routine": rt.Name, "reason": "already_running", "open_work": open})
			}
			created, err := s.createWorkTx(ctx, tx, workRequest{Routine: rt.Name, trigger: model.TriggerSchedule})
			if err != nil {
				return err
			}
			return tx.Journal(ctx, "schedule.fired", store.EntityWork, created.Work.ID, map[string]any{"routine": rt.Name, "next_due_at": next})
		})
		if err != nil {
			s.log.ErrorContext(ctx, "fire scheduled routine", "routine", rt.Name, "error", err)
			continue
		}
		s.log.InfoContext(ctx, "scheduled routine fired", "routine", rt.Name, "skipped", open > 0, "next_due_at", next)
	}
}

// fireDueWorkflowRoutine fires one workflow-target trigger routine: the
// routine owns the schedule, the workflow owns the graph. Skip-if-running
// keys on the workflow's open runs, the same policy as a workflow's own
// schedule.
func (s *Server) fireDueWorkflowRoutine(ctx context.Context, rt store.Routine, workflow string, now time.Time) {
	open, err := s.store.HasOpenWorkflowRun(ctx, workflow)
	if err != nil {
		s.log.ErrorContext(ctx, "check open runs", "routine", rt.Name, "workflow", workflow, "error", err)
		return
	}
	next, err := store.NextOccurrence(rt.Schedule, now)
	if err != nil {
		s.log.WarnContext(ctx, "unparseable routine schedule", "routine", rt.Name, "error", err)
		return
	}
	var runID string
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		saved, err := tx.GetRoutine(ctx, rt.Name)
		if err != nil || !saved.ScheduleEnabled || !saved.ArchivedAt.IsZero() {
			return err
		}
		if err := tx.SetNextDue(ctx, rt.Name, next); err != nil {
			return err
		}
		if open {
			return tx.Journal(ctx, "schedule.skipped", store.EntityDaemon, saved.ID, map[string]any{"routine": rt.Name, "workflow": workflow, "reason": "already_running"})
		}
		wf, err := tx.GetWorkflow(ctx, workflow)
		if err != nil {
			return err
		}
		if !wf.ArchivedAt.IsZero() {
			return tx.Journal(ctx, "schedule.skipped", store.EntityDaemon, saved.ID, map[string]any{"routine": rt.Name, "workflow": workflow, "reason": "workflow_archived"})
		}
		run := &store.WorkflowRun{
			WorkflowID: wf.ID, WorkflowName: wf.Name, WorkflowGeneration: wf.Generation, Graph: wf.Graph,
			Trigger: model.TriggerSchedule, Context: store.RunContext{Repositories: saved.Repositories, Objective: saved.Objective},
		}
		if err := tx.CreateWorkflowRun(ctx, run); err != nil {
			return err
		}
		runID = run.ID
		return tx.Journal(ctx, "schedule.fired", store.EntityWorkflow, run.ID, map[string]any{"routine": rt.Name, "workflow": workflow, "next_due_at": next})
	})
	if err != nil {
		s.log.ErrorContext(ctx, "fire scheduled workflow routine", "routine", rt.Name, "workflow", workflow, "error", err)
		return
	}
	if runID != "" {
		s.advanceRun(ctx, runID)
	}
	s.log.InfoContext(ctx, "scheduled routine fired workflow", "routine", rt.Name, "workflow", workflow, "run_id", runID, "skipped", open, "next_due_at", next)
}

// fireDueScriptRoutine fires one script-target trigger: a synthetic
// single-node run, skip-if-running keyed on the script's open runs.
func (s *Server) fireDueScriptRoutine(ctx context.Context, rt store.Routine, script string, now time.Time) {
	open, err := s.store.HasOpenWorkflowRun(ctx, "script:"+script)
	if err != nil {
		s.log.ErrorContext(ctx, "check open script runs", "routine", rt.Name, "script", script, "error", err)
		return
	}
	next, err := store.NextOccurrence(rt.Schedule, now)
	if err != nil {
		s.log.WarnContext(ctx, "unparseable routine schedule", "routine", rt.Name, "error", err)
		return
	}
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		saved, err := tx.GetRoutine(ctx, rt.Name)
		if err != nil || !saved.ScheduleEnabled || !saved.ArchivedAt.IsZero() {
			return err
		}
		if err := tx.SetNextDue(ctx, rt.Name, next); err != nil {
			return err
		}
		if open {
			return tx.Journal(ctx, "schedule.skipped", store.EntityDaemon, saved.ID, map[string]any{"routine": rt.Name, "script": script, "reason": "already_running"})
		}
		return nil
	})
	if err != nil {
		s.log.ErrorContext(ctx, "fire scheduled script routine", "routine", rt.Name, "script", script, "error", err)
		return
	}
	if open {
		s.log.InfoContext(ctx, "scheduled script routine skipped", "routine", rt.Name, "script", script, "next_due_at", next)
		return
	}
	run, err := s.startScriptRun(ctx, script, rt.Repositories, rt.Objective, model.TriggerSchedule)
	if err != nil {
		s.log.ErrorContext(ctx, "start scheduled script run", "routine", rt.Name, "script", script, "error", err)
		return
	}
	s.log.InfoContext(ctx, "scheduled routine fired script", "routine", rt.Name, "script", script, "run_id", run.ID, "next_due_at", next)
}

// fireDueWorkflows starts one run per due workflow, unless a run is still
// going.
func (s *Server) fireDueWorkflows(ctx context.Context, now time.Time) {
	due, err := s.store.DueWorkflows(ctx, now)
	if err != nil {
		s.log.ErrorContext(ctx, "list due workflows", "error", err)
		return
	}
	for _, wf := range due {
		open, err := s.store.HasOpenWorkflowRun(ctx, wf.Name)
		if err != nil {
			s.log.ErrorContext(ctx, "check open runs", "workflow", wf.Name, "error", err)
			continue
		}
		next, err := store.NextOccurrence(wf.Schedule, now)
		if err != nil {
			s.log.WarnContext(ctx, "unparseable workflow schedule", "workflow", wf.Name, "error", err)
			continue
		}
		var runID string
		err = s.store.Write(ctx, func(tx *store.Tx) error {
			saved, err := tx.GetWorkflow(ctx, wf.Name)
			if err != nil || !saved.ScheduleEnabled || !saved.ArchivedAt.IsZero() {
				return err
			}
			if err := tx.SetWorkflowNextDue(ctx, wf.Name, next); err != nil {
				return err
			}
			if open {
				return tx.Journal(ctx, "schedule.skipped", store.EntityWorkflow, saved.ID, map[string]any{"workflow": wf.Name, "reason": "already_running"})
			}
			run := &store.WorkflowRun{
				WorkflowID: saved.ID, WorkflowName: saved.Name, WorkflowGeneration: saved.Generation,
				Graph: saved.Graph, Trigger: model.TriggerSchedule,
			}
			if err := tx.CreateWorkflowRun(ctx, run); err != nil {
				return err
			}
			runID = run.ID
			return tx.Journal(ctx, "schedule.fired", store.EntityWorkflow, run.ID, map[string]any{"workflow": wf.Name, "next_due_at": next})
		})
		if err != nil {
			s.log.ErrorContext(ctx, "fire scheduled workflow", "workflow", wf.Name, "error", err)
			continue
		}
		if runID != "" {
			s.advanceRun(ctx, runID)
		}
		s.log.InfoContext(ctx, "scheduled workflow fired", "workflow", wf.Name, "run_id", runID, "skipped", open, "next_due_at", next)
	}
}
