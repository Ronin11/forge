package web

import (
	"context"
	"encoding/json"
	"strings"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/engine"
	"forge/internal/core/model"
	"forge/internal/core/store"
)

// Capacity-aware learning (DESIGN §10 turned into incentives): subscription
// tokens are prepaid and expire — the 5h/7d windows measured by BudgetPolicy
// are use-it-or-lose-it. When both windows run meaningfully behind their
// burn-down pace (u < target·f − margin), headroom is about to be wasted, and
// the loop should SPEND it: the opportunist tops up under-run live
// experiments, and the escalation ladder's backlog ceiling relaxes ("fable is
// free right now"). When the windows are busy, everything defers through the
// ordinary admission rules and none of this fires.

// surplusMargin is how far behind the burn-down pace both windows must be
// before headroom counts as surplus.
const surplusMargin = 0.10

// opportunistCooldown throttles surplus-triggered firings; in-memory, so a
// restart may fire one early — harmless, the skip-if-open rules still hold.
const opportunistCooldown = 2 * time.Hour

// capacityReporter is the slice of the budget policy this file needs; the
// M3 BudgetPolicy implements it (structurally — see usageReporter).
type capacityReporter interface {
	Usage(ctx context.Context) (engine.Usage, error)
	Config() config.BudgetConfig
}

// capacitySurplus reports whether every sampled window sits below its
// burn-down line by at least margin — prepaid capacity on track to expire
// unused. No sampled window (a fresh install) is never surplus.
func (s *Engine) capacitySurplus(ctx context.Context, margin float64) bool {
	rp, ok := s.policy.(capacityReporter)
	if !ok {
		return false
	}
	u, err := rp.Usage(ctx)
	if err != nil {
		return false
	}
	cfg := rp.Config()
	sampled := false
	for _, e := range []struct {
		w      engine.WindowUsage
		target float64
	}{{u.FiveHour, cfg.FiveHourTarget}, {u.SevenDay, cfg.SevenDayTarget}} {
		if e.w.Utilization < 0 {
			continue
		}
		sampled = true
		if e.w.Utilization >= e.target*e.w.FractionElapsed-margin {
			return false
		}
	}
	return sampled
}

// opportunisticLearning is the sweep-tick demand generator: on surplus, top
// up a live experiment whose arms are short of min_runs by firing its
// subject directive's trigger routine — the run enrolls through the ordinary
// assignment path. One firing per cooldown, skipped while the routine has
// open work, journaled either way it fires.
func (s *Server) opportunisticLearning(ctx context.Context) {
	s.liveMu.Lock()
	last := s.lastOpportunist
	s.liveMu.Unlock()
	if !last.IsZero() && s.now().Sub(last) < opportunistCooldown {
		return
	}
	if !s.capacitySurplus(ctx, surplusMargin) {
		return
	}
	rows, err := s.store.LiveExperiments(ctx)
	if err != nil {
		s.log.ErrorContext(ctx, "opportunist: live experiments", "error", err)
		return
	}
	for i := range rows {
		row := &rows[i]
		if row.Status != store.ExperimentLive {
			continue
		}
		name, ok := strings.CutPrefix(row.Subject, "directive:")
		if !ok {
			continue
		}
		short, err := s.experimentArmsShort(ctx, row)
		if err != nil || !short {
			continue
		}
		rt, err := s.routineTargeting(ctx, "directive:"+name)
		if err != nil || rt == nil {
			continue
		}
		open, err := s.store.OpenWorkCountForRoutine(ctx, rt.ID)
		if err != nil || open > 0 {
			continue
		}
		var created workCreated
		err = s.store.Write(ctx, func(tx *store.Tx) error {
			var werr error
			created, werr = s.createWorkTx(ctx, tx, workRequest{
				Routine: rt.Name, Force: true, trigger: model.TriggerSchedule, submittedBy: "learning:opportunist",
			})
			if werr != nil {
				return werr
			}
			return tx.Journal(ctx, "learning.opportunist_fired", store.EntityWork, created.Work.ID, map[string]any{
				"experiment": row.ID, "routine": rt.Name, "reason": "window surplus, arm short of min_runs"})
		})
		if err != nil {
			s.log.WarnContext(ctx, "opportunist: fire routine", "routine", rt.Name, "error", err)
			return
		}
		s.liveMu.Lock()
		s.lastOpportunist = s.now()
		s.liveMu.Unlock()
		s.log.InfoContext(ctx, "opportunistic learning fired", "experiment", row.ID, "routine", rt.Name, "work_id", created.Work.ID)
		return
	}
}

// experimentArmsShort reports whether any arm of a live experiment has fewer
// facts than min_runs — the condition a top-up run relieves.
func (s *Engine) experimentArmsShort(ctx context.Context, row *store.Experiment) (bool, error) {
	tallies, err := s.liveExperimentTallies(ctx, row.ID, row.MinRuns)
	if err != nil {
		return false, err
	}
	if len(tallies) == 0 {
		return true, nil
	}
	seen := map[string]int{}
	for _, t := range tallies {
		seen[t.Label] = t.Runs
	}
	var arms []store.ExperimentArm
	if err := json.Unmarshal(row.Arms, &arms); err != nil {
		return false, err
	}
	for _, a := range arms {
		if seen[a.Label] < row.MinRuns {
			return true, nil
		}
	}
	return false, nil
}

// routineTargeting finds the unarchived trigger routine for a target string.
func (s *Engine) routineTargeting(ctx context.Context, target string) (*store.Routine, error) {
	routines, err := s.store.ListRoutines(ctx, false)
	if err != nil {
		return nil, err
	}
	for i := range routines {
		if routines[i].Target == target {
			return &routines[i], nil
		}
	}
	return nil, nil
}
