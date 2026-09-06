package web

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// The assessment cadence (operator-approved 2026-09-06): every scored bench
// gets walked by the user personas, and every finished trial gets a product
// review that turns the evidence into a prioritized batch. Hand-fired twice
// with results worth keeping (the trial found a correctness bug supervise
// never saw; the review turned it into a repaired product) — now standing
// infrastructure. Journal markers make each step fire exactly once; a
// missing user-trial or product-review routine disables the cadence quietly.

// Wide enough to survive daemon downtime, narrow enough that a fresh deploy
// never backfills history: the feature's first tick fired reviews for every
// root in a 14-day window (2026-09-06), five of them stale.
const cadenceWindow = 36 * time.Hour

func (s *Server) assessmentCadence(ctx context.Context) {
	roots, err := s.store.SettledBenchRoots(ctx, s.now().Add(-cadenceWindow))
	if err != nil {
		s.log.ErrorContext(ctx, "cadence: settled benches", "error", err)
		return
	}
	for _, rootID := range roots {
		s.cadenceStep(ctx, rootID)
	}
}

func (s *Server) cadenceStep(ctx context.Context, rootID string) {
	root, err := s.store.GetWork(ctx, rootID)
	if err != nil {
		return
	}
	targets, err := s.store.TargetsForWork(ctx, rootID)
	if err != nil || len(targets) == 0 {
		return
	}
	repo := targets[0].Repository
	if repo == "" {
		return
	}
	// Step 1: the trial.
	fired, err := s.cadenceOnce(ctx, rootID, "cadence.trial_fired", func(tx *store.Tx) (string, error) {
		if _, err := tx.GetRoutine(ctx, "user-trial"); err != nil {
			return "", err // routine absent: cadence off
		}
		created, err := s.createWorkTx(ctx, tx, workRequest{
			Routine: "user-trial", Repositories: []string{repo}, Force: true, trigger: model.TriggerSchedule,
			CausedBy: rootID, cause: model.CauseFollowUp, submittedBy: "cadence:trial",
			Objective: "Trial the app this benchmark built (" + root.Title + ").",
		})
		if err != nil {
			return "", err
		}
		return created.Work.ID, nil
	})
	if err != nil || fired {
		return // freshly fired (or broken): the review waits for the trial
	}
	// Step 2: the review, once the trial settled with a result.
	trialID, trialDone, summary := s.cadenceTrialState(ctx, rootID)
	if trialID == "" || !trialDone {
		return
	}
	_, _ = s.cadenceOnce(ctx, rootID, "cadence.review_fired", func(tx *store.Tx) (string, error) {
		if _, err := tx.GetRoutine(ctx, "product-review"); err != nil {
			return "", err
		}
		assessments, _ := s.store.RecentAssessments(ctx, s.now().Add(-7*24*time.Hour), 6)
		var b strings.Builder
		b.WriteString("USER TRIAL REPORT for " + root.Title + ":\n" + summary + "\n\nRecent supervise assessments:\n")
		for _, a := range assessments {
			fmt.Fprintf(&b, "- %s (%s, round %d): %s\n", a.RootTitle, a.Outcome, a.Round, a.Weakness)
		}
		created, err := s.createWorkTx(ctx, tx, workRequest{
			Routine: "product-review", Repositories: []string{repo}, Force: true, trigger: model.TriggerSchedule,
			CausedBy: trialID, cause: model.CauseFollowUp, submittedBy: "cadence:review",
			// The review's repair batch must land on the integration branch:
			// without this the whole child tree strands on task branches —
			// three rounds of correct, unmerged work on 20260905-1009
			// (proposal 543c254d's 83%-missing-deliverables pattern).
			Integrate: true,
			Objective: b.String(),
		})
		if err != nil {
			return "", err
		}
		return created.Work.ID, nil
	})
}

// cadenceOnce runs the step unless its marker exists; fired=true when this
// call created the work.
func (s *Server) cadenceOnce(ctx context.Context, rootID, marker string, step func(tx *store.Tx) (string, error)) (bool, error) {
	fired := false
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		done, err := tx.HasJournal(ctx, store.EntityWork, rootID, marker)
		if err != nil || done {
			return err
		}
		id, err := step(tx)
		if err != nil {
			return err
		}
		fired = true
		return tx.Journal(ctx, marker, store.EntityWork, rootID, map[string]any{"work_id": id})
	})
	if err != nil {
		s.log.DebugContext(ctx, "cadence step skipped", "root", rootID, "marker", marker, "error", err)
	}
	return fired, err
}

// cadenceTrialState finds the cadence-fired trial for a bench root and, when
// settled, its result summary.
func (s *Server) cadenceTrialState(ctx context.Context, rootID string) (id string, done bool, summary string) {
	tree, err := s.store.WorkTree(ctx, rootID)
	if err != nil {
		return "", false, ""
	}
	for _, w := range tree {
		if w.SubmittedBy != "cadence:trial" {
			continue
		}
		id = w.ID
		if w.FinishedAt.IsZero() {
			return id, false, ""
		}
		if run, err := s.store.LearningRun(ctx, w.ID); err == nil && run != nil {
			var env struct {
				Summary string `json:"summary"`
			}
			if json.Unmarshal(run.Result, &env) == nil {
				summary = env.Summary
			}
		}
		return id, true, summary
	}
	return "", false, ""
}
