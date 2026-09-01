package store

import (
	"context"
	"errors"
	"testing"

	"forge/internal/model"
)

// retryWork creates one Work with one Target and drives the Target along
// path; opts applies to the final step (the failure reason, typically).
func retryWork(t *testing.T, s *Store, integrate bool, path []model.State, final TransitionOptions) *Target {
	t.Helper()
	ctx := context.Background()
	w := &Work{RoutineName: "r", Title: "t", Trigger: model.TriggerManual,
		BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto, Integrate: integrate, SubmittedBy: "test"}
	var target Target
	err := s.Write(ctx, func(tx *Tx) error {
		targets, err := tx.CreateWork(ctx, w, []string{"repo1"}, nil)
		if err != nil {
			return err
		}
		target = targets[0]
		for i, st := range path {
			opts := TransitionOptions{Actor: "test"}
			if i == len(path)-1 {
				opts = final
			}
			if _, err := tx.Transition(ctx, target.ID, st, opts); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return &target
}

func TestRetryTargetFromTerminalStates(t *testing.T) {
	ctx := context.Background()
	cases := []struct {
		name  string
		path  []model.State
		final TransitionOptions
	}{
		{"failed", []model.State{model.Claimed, model.Failed}, TransitionOptions{Actor: "test", Reason: model.ReasonExitNonzero}},
		{"unverified", []model.State{model.Claimed, model.Preparing, model.Running, model.Verifying, model.Unverified}, TransitionOptions{Actor: "test", UnverifiedReason: "checks_failed"}},
		{"cancelled", []model.State{model.Cancelled}, TransitionOptions{Actor: "test", Reason: model.ReasonCancelled}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			s := openTest(t)
			target := retryWork(t, s, false, c.path, c.final)
			var got *Target
			err := s.Write(ctx, func(tx *Tx) error {
				var err error
				got, err = tx.RetryTarget(ctx, target.ID, "")
				return err
			})
			if err != nil {
				t.Fatalf("RetryTarget: %v", err)
			}
			if got.State != model.Pending || got.FailureReason != "" || got.UnverifiedReason != "" || got.WorkerID != "" || !got.FinishedAt.IsZero() {
				t.Errorf("returned target = %+v", got)
			}
			fresh, err := s.GetTarget(ctx, target.ID)
			if err != nil {
				t.Fatal(err)
			}
			if fresh.State != model.Pending || fresh.FailureReason != "" || fresh.UnverifiedReason != "" || fresh.WorkerID != "" || !fresh.FinishedAt.IsZero() {
				t.Errorf("stored target = %+v", fresh)
			}
			w, err := s.GetWork(ctx, target.WorkID)
			if err != nil {
				t.Fatal(err)
			}
			if !w.FinishedAt.IsZero() {
				t.Errorf("work still finished at %v", w.FinishedAt)
			}
			rows, err := s.JournalForEntity(ctx, EntityTarget, target.ID)
			if err != nil {
				t.Fatal(err)
			}
			found := false
			for _, r := range rows {
				if r.Kind == "target.retried" {
					found = true
				}
			}
			if !found {
				t.Error("no target.retried journal row")
			}
		})
	}
}

func TestReverifyTargetFromUnverified(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	target := retryWork(t, s, false,
		[]model.State{model.Claimed, model.Preparing, model.Running, model.Verifying, model.Unverified},
		TransitionOptions{Actor: "test", UnverifiedReason: "verify_attempt_failed"})
	var got *Target
	err := s.Write(ctx, func(tx *Tx) error {
		var err error
		got, err = tx.ReverifyTarget(ctx, target.ID)
		return err
	})
	if err != nil {
		t.Fatalf("ReverifyTarget: %v", err)
	}
	if got.State != model.Verifying || got.UnverifiedReason != "" || !got.FinishedAt.IsZero() {
		t.Errorf("returned target = %+v", got)
	}
	fresh, err := s.GetTarget(ctx, target.ID)
	if err != nil {
		t.Fatal(err)
	}
	if fresh.State != model.Verifying || fresh.UnverifiedReason != "" || !fresh.FinishedAt.IsZero() {
		t.Errorf("stored target = %+v", fresh)
	}
	w, err := s.GetWork(ctx, target.WorkID)
	if err != nil {
		t.Fatal(err)
	}
	if !w.FinishedAt.IsZero() {
		t.Errorf("work still finished at %v", w.FinishedAt)
	}
	rows, err := s.JournalForEntity(ctx, EntityTarget, target.ID)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, r := range rows {
		if r.Kind == "target.reverified" {
			found = true
		}
	}
	if !found {
		t.Error("no target.reverified journal row")
	}
}

func TestReverifyTargetRefusals(t *testing.T) {
	ctx := context.Background()
	cases := []struct {
		name string
		path []model.State
	}{
		{"failed", []model.State{model.Claimed, model.Failed}},
		{"succeeded", []model.State{model.Claimed, model.Preparing, model.Running, model.Verifying, model.Succeeded}},
		{"cancelled", []model.State{model.Cancelled}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			s := openTest(t)
			target := retryWork(t, s, false, c.path, TransitionOptions{Actor: "test"})
			err := s.Write(ctx, func(tx *Tx) error {
				_, err := tx.ReverifyTarget(ctx, target.ID)
				return err
			})
			if !errors.Is(err, model.ErrTransition) {
				t.Fatalf("ReverifyTarget from %s: err = %v, want ErrTransition", c.name, err)
			}
		})
	}
}

func TestRetryTargetRefusals(t *testing.T) {
	ctx := context.Background()
	cases := []struct {
		name      string
		integrate bool
		path      []model.State
	}{
		{"running", false, []model.State{model.Claimed, model.Preparing, model.Running}},
		{"succeeded", false, []model.State{model.Claimed, model.Preparing, model.Running, model.Verifying, model.Succeeded}},
		{"merged", true, []model.State{model.Claimed, model.Preparing, model.Running, model.Verifying, model.Succeeded, model.QueuedForMerge, model.Merging, model.Merged}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			s := openTest(t)
			target := retryWork(t, s, c.integrate, c.path, TransitionOptions{Actor: "test"})
			err := s.Write(ctx, func(tx *Tx) error {
				_, err := tx.RetryTarget(ctx, target.ID, "")
				return err
			})
			if !errors.Is(err, model.ErrTransition) {
				t.Fatalf("RetryTarget from %s: err = %v, want ErrTransition", c.name, err)
			}
		})
	}
}

func TestRetryTargetModelOverrideRefused(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	target := retryWork(t, s, false, []model.State{model.Claimed, model.Failed}, TransitionOptions{Actor: "test", Reason: model.ReasonExitNonzero})
	err := s.Write(ctx, func(tx *Tx) error {
		_, err := tx.RetryTarget(ctx, target.ID, "sonnet")
		return err
	})
	if !errors.Is(err, ErrModelOverride) {
		t.Fatalf("err = %v, want ErrModelOverride", err)
	}
	fresh, err := s.GetTarget(ctx, target.ID)
	if err != nil {
		t.Fatal(err)
	}
	if fresh.State != model.Failed {
		t.Errorf("state changed to %s on a refused retry", fresh.State)
	}
}

func TestRetryTargetNotFound(t *testing.T) {
	ctx := context.Background()
	s := openTest(t)
	err := s.Write(ctx, func(tx *Tx) error {
		_, err := tx.RetryTarget(ctx, "00000000000000000000000000000000", "")
		return err
	})
	if !errors.Is(err, ErrNotFound) {
		t.Fatalf("err = %v, want ErrNotFound", err)
	}
}

// A finished attempt is never rebound by a later claim: retried targets get a
// fresh attempt while a waiting_human resume (finished_at cleared by the
// answer) still rebinds.
func TestAttemptForTargetSkipsFinished(t *testing.T) {
	st := openTest(t)
	ctx := context.Background()
	target := retryWork(t, st, false, []model.State{model.Claimed, model.Failed}, TransitionOptions{Actor: "test", Reason: model.ReasonExitNonzero})
	id := model.NewID()
	if err := st.Write(ctx, func(tx *Tx) error {
		if _, err := tx.Exec(ctx, `INSERT INTO attempts (id, target_id, worker_id, claim_request_id, mcp_token_hash, executor, model, model_alias, mode, autonomy, finished_at, created_at, updated_at) VALUES (?, ?, 'w', 'cr1', 'h', 'e', 'm', 'haiku', 'run', 'auto', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')`, id, target.ID); err != nil {
			return err
		}
		a, err := tx.attemptForTarget(ctx, target.ID)
		if err != nil {
			return err
		}
		if a != nil {
			t.Errorf("finished attempt rebindable: %+v", a)
		}
		if _, err := tx.Exec(ctx, `UPDATE attempts SET finished_at = NULL WHERE id = ?`, id); err != nil {
			return err
		}
		a, err = tx.attemptForTarget(ctx, target.ID)
		if err != nil {
			return err
		}
		if a == nil || a.ID != id {
			t.Errorf("unfinished attempt not found: %+v", a)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}
