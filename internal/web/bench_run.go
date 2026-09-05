package web

import (
	"context"
	"fmt"
	"net/http"
	"path/filepath"
	"time"

	"forge/internal/core/bench"
	"forge/internal/core/model"
	"forge/internal/core/store"
)

// Daemon-fired benchmark runs: the same spec → throwaway repo → plan-mode
// root flow the bench CLI performs, executed server-side so a bench-target
// trigger routine's schedule (or POST /api/v1/bench/{name}/run) can drive
// the improvement loop without anyone babysitting a shell script. Bench
// spend is learning spend: an exhausted [learning] pool skips the firing.

// startBenchRun creates one benchmark run for the named spec.
func (s *Server) startBenchRun(ctx context.Context, name string, trigger model.Trigger) (workCreated, error) {
	if s.benchCfg.SpecsDir == "" {
		return workCreated{}, badRequest("[bench] specs_dir is not configured; run `forge bench run` from a checkout instead")
	}
	if err := model.ValidateName(name); err != nil {
		return workCreated{}, badRequest("bench name: %v", err)
	}
	if s.addRepo == nil {
		return workCreated{}, badRequest("this process cannot add repositories")
	}
	spec, err := bench.LoadSpec(filepath.Join(s.benchCfg.SpecsDir, name+".md"))
	if err != nil {
		return workCreated{}, badRequest("%v", err)
	}
	if s.learningCfg.BudgetUSDPerWeek > 0 {
		spent, err := s.store.LearningSpendSince(ctx, s.now().Add(-7*24*time.Hour))
		if err != nil {
			return workCreated{}, err
		}
		if spent >= s.learningCfg.BudgetUSDPerWeek {
			if jerr := s.store.Write(ctx, func(tx *store.Tx) error {
				return tx.Journal(ctx, "learning.budget_exhausted", store.EntityDaemon, "", map[string]any{
					"spent_usd": spent, "budget_usd": s.learningCfg.BudgetUSDPerWeek, "bench": name})
			}); jerr != nil {
				return workCreated{}, jerr
			}
			return workCreated{}, fmt.Errorf("learning budget exhausted ($%.2f of $%.2f this week): %w", spent, s.learningCfg.BudgetUSDPerWeek, store.ErrConflict)
		}
	}
	parent := s.benchCfg.RepoParent
	if parent == "" {
		parent = filepath.Join(s.home, "..", "Projects")
	}
	repoName := fmt.Sprintf("bench-%s-%s", name, s.now().Format("20060102-1504"))
	repoPath := filepath.Join(parent, repoName)
	if err := bench.InitRepo(ctx, repoPath); err != nil {
		return workCreated{}, err
	}
	repo, err := s.addRepo(ctx, repoPath, "", "")
	if err != nil {
		return workCreated{}, fmt.Errorf("register %s: %w", repoPath, err)
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error {
		return tx.UpsertProvisionalRepository(ctx, repo)
	}); err != nil {
		return workCreated{}, err
	}
	var created workCreated
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		var werr error
		created, werr = s.createWorkTx(ctx, tx, workRequest{
			Prompt: spec.Body, Repositories: []string{repo.Name}, Mode: "plan",
			Size: spec.Size, Model: spec.Model, Autonomy: model.Autonomy(spec.Autonomy),
			MaxTurns: &spec.MaxTurns, TimeoutSeconds: &spec.Timeout,
			Integrate: true, BenchName: name, Title: "bench: " + name, Force: true,
			trigger: trigger,
		})
		return werr
	})
	if err != nil {
		return workCreated{}, err
	}
	s.log.InfoContext(ctx, "bench run started", "bench", name, "work_id", created.Work.ID, "repository", repo.Name, "trigger", trigger)
	return created, nil
}

// runBench is POST /api/v1/bench/{name}/run.
func (s *Server) runBench(r *http.Request) (int, any, error) {
	if s.Draining() {
		return 0, nil, errDraining
	}
	created, err := s.startBenchRun(r.Context(), r.PathValue("name"), model.TriggerManual)
	if err != nil {
		return 0, nil, err
	}
	return http.StatusCreated, created, nil
}

// fireDueBenchRoutine fires one bench-target trigger routine: a fresh
// throwaway run of the spec, skipped while the last run's tree is still
// open (the tree is the unit — a bench with unsettled works is running).
func (s *Server) fireDueBenchRoutine(ctx context.Context, rt store.Routine, name string, now time.Time) {
	open, err := s.store.OpenWorkCountForSubmitter(ctx, "bench:"+name)
	if err != nil {
		s.log.ErrorContext(ctx, "count open bench work", "routine", rt.Name, "bench", name, "error", err)
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
		if open > 0 {
			return tx.Journal(ctx, "schedule.skipped", store.EntityDaemon, saved.ID, map[string]any{"routine": rt.Name, "bench": name, "reason": "already_running", "open_work": open})
		}
		return nil
	})
	if err != nil {
		s.log.ErrorContext(ctx, "fire scheduled bench routine", "routine", rt.Name, "bench", name, "error", err)
		return
	}
	if open > 0 {
		s.log.InfoContext(ctx, "scheduled bench skipped", "routine", rt.Name, "bench", name, "open_work", open, "next_due_at", next)
		return
	}
	created, err := s.startBenchRun(ctx, name, model.TriggerSchedule)
	if err != nil {
		// An exhausted learning pool or a spec problem is journaled/logged;
		// the schedule already advanced, so the next occurrence retries.
		s.log.WarnContext(ctx, "scheduled bench not started", "routine", rt.Name, "bench", name, "error", err)
		return
	}
	s.log.InfoContext(ctx, "scheduled bench fired", "routine", rt.Name, "bench", name, "work_id", created.Work.ID, "next_due_at", next)
}
