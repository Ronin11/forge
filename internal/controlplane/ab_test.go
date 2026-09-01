package controlplane

import (
	"encoding/json"
	"fmt"
	"path/filepath"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/store"
)

// abFixture is the world checkABReverts sees: a routine on generation 2 via an
// applied proposal, with facts seeded per generation.
type abFixture struct {
	t     *testing.T
	st    *store.Store
	srv   *Server
	clock *fakeClock
	seq   int
}

func newABFixture(t *testing.T) *abFixture {
	t.Helper()
	clock := &fakeClock{now: time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)}
	st, err := store.Open(actx(), filepath.Join(t.TempDir(), "forge.sqlite3"), store.Options{Clock: clock.Now})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	})
	f := &abFixture{t: t, st: st, clock: clock}
	f.write(func(tx *store.Tx) error {
		if err := tx.EnsureProject(actx(), "default"); err != nil {
			return err
		}
		if err := tx.Register(actx(), protocol.RegisterRequest{WorkerID: testWorkerID, Name: "laptop", Version: "test", MaxConcurrent: 2,
			Executors:    []string{"claude-code"},
			Repositories: []protocol.Repository{{Name: "equitizr", Path: "/tmp/equitizr", OriginIdentity: "github.com/x/equitizr"}}}); err != nil {
			return err
		}
		return tx.CreateRoutine(actx(), &store.Routine{Name: "inventory", Mode: "run", Prompt: "old prompt", Model: "haiku",
			TimeoutSeconds: 300, Repositories: []string{"equitizr"}})
	})
	srv, err := NewServer(ServerOptions{Store: st, Clock: clock.Now, Version: "test", TransportOverride: transportUnix, Home: t.TempDir()})
	if err != nil {
		t.Fatal(err)
	}
	f.srv = srv
	return f
}

func (f *abFixture) write(fn func(tx *store.Tx) error) {
	f.t.Helper()
	if err := f.st.Write(actx(), fn); err != nil {
		f.t.Fatal(err)
	}
}

// appliedProposal approves and applies one routine proposal, moving the
// routine to generation 2 with the new prompt.
func (f *abFixture) appliedProposal() *store.Proposal {
	f.t.Helper()
	p := &store.Proposal{Source: "manual", Kind: model.ProposalRoutine, Target: "routine:inventory",
		After: json.RawMessage(`{"prompt":"new prompt"}`), Rationale: "r", VerificationPlan: "v"}
	f.write(func(tx *store.Tx) error {
		if err := tx.CreateProposal(actx(), p); err != nil {
			return err
		}
		if _, err := tx.DecideProposal(actx(), p.ID, model.ProposalApproved, "nate"); err != nil {
			return err
		}
		ref, err := f.srv.applyProposal(actx(), tx, p)
		if err != nil {
			return err
		}
		_, err = tx.MarkProposalApplied(actx(), p.ID, ref)
		return err
	})
	return p
}

// attempt creates and claims one attempt so a facts row has its foreign key.
func (f *abFixture) attempt() *store.Attempt {
	f.t.Helper()
	f.seq++
	req := fmt.Sprintf("ab-%d", f.seq)
	w := &store.Work{RoutineName: "inventory", Generation: 1, Title: "run " + req, Trigger: model.TriggerManual,
		Snapshot: []byte(`{}`), Priority: 100, BudgetClass: model.ClassNormal, Autonomy: model.AutonomyAuto}
	var targets []store.Target
	f.write(func(tx *store.Tx) error {
		var err error
		targets, err = tx.CreateWork(actx(), w, []string{"equitizr"}, nil)
		return err
	})
	var a *store.Attempt
	f.write(func(tx *store.Tx) error {
		var err error
		a, err = tx.Claim(actx(), store.ClaimParams{TargetID: targets[0].ID, WorkerID: testWorkerID, ClaimRequestID: req,
			LeaseToken: "lease-" + req, MCPToken: "mcp-" + req, Executor: "claude-code", Model: "claude-haiku-4-5",
			ModelAlias: "haiku", Mode: "run", Autonomy: model.AutonomyAuto})
		return err
	})
	return a
}

// fact seeds one facts row: a verified success or a failure, with a cost.
func (f *abFixture) fact(gen int, verified bool, cost float64, finished time.Time) {
	f.t.Helper()
	a := f.attempt()
	state, pass := model.Failed, (*bool)(nil)
	if verified {
		v := true
		state, pass = model.Succeeded, &v
	}
	f.write(func(tx *store.Tx) error {
		return tx.InsertFacts(actx(), &store.AttemptFacts{AttemptID: a.ID, TargetID: a.TargetID, Routine: "inventory",
			Generation: gen, Project: "default", Repository: "equitizr", Worker: testWorkerID, Executor: "claude-code",
			Model: "haiku", Mode: "run", Trigger: model.TriggerManual, Autonomy: model.AutonomyAuto,
			FinishedAt: finished, State: state, VerificationPass: pass, CostUSD: &cost})
	})
}

func (f *abFixture) outcome(p *store.Proposal) (model.ProposalStatus, abOutcome) {
	f.t.Helper()
	got, err := f.st.GetProposal(actx(), p.ID)
	if err != nil {
		f.t.Fatal(err)
	}
	var o abOutcome
	if len(got.OutcomeMetrics) > 0 {
		if err := json.Unmarshal(got.OutcomeMetrics, &o); err != nil {
			f.t.Fatal(err)
		}
	}
	return got.Status, o
}

func TestABRevertOnRateRegression(t *testing.T) {
	f := newABFixture(t)
	base := f.clock.Now()
	p := f.appliedProposal()
	// Generation 1: an older failure, then two verified successes. With K=2
	// only the two newest rows count — proving newest-first with the limit.
	f.fact(1, false, 1.0, base.Add(-3*time.Hour))
	f.fact(1, true, 1.0, base.Add(-2*time.Hour))
	f.fact(1, true, 1.0, base.Add(-1*time.Hour))
	// Generation 2: two failures — verified rate 0 against 1.0.
	f.fact(2, false, 1.0, base.Add(1*time.Hour))
	f.fact(2, false, 1.0, base.Add(2*time.Hour))

	f.srv.checkABReverts(actx(), ReflectionConfig{K: 2, Margin: 0.2})

	r, err := f.st.GetRoutine(actx(), "inventory")
	if err != nil {
		t.Fatal(err)
	}
	if r.Generation != 3 || r.Prompt != "old prompt" {
		t.Errorf("routine after revert = generation %d prompt %q, want 3 / old prompt", r.Generation, r.Prompt)
	}
	status, o := f.outcome(p)
	if status != model.ProposalReverted {
		t.Fatalf("proposal status = %s, want reverted", status)
	}
	if o.RegressedOn != "rate" || o.K != 2 || o.NewRate != 0 || o.PrevRate != 1 || o.RestoredGeneration != 1 || o.NewGeneration != 3 {
		t.Errorf("outcome = %+v", o)
	}
}

func TestABRevertOnCostRegression(t *testing.T) {
	f := newABFixture(t)
	base := f.clock.Now()
	p := f.appliedProposal()
	// Equal rates (all verified successes); cost per success 1.0 → 2.0, which
	// is beyond the 20% margin.
	f.fact(1, true, 1.0, base.Add(-2*time.Hour))
	f.fact(1, true, 1.0, base.Add(-1*time.Hour))
	f.fact(2, true, 2.0, base.Add(1*time.Hour))
	f.fact(2, true, 2.0, base.Add(2*time.Hour))

	f.srv.checkABReverts(actx(), ReflectionConfig{K: 2, Margin: 0.2})

	status, o := f.outcome(p)
	if status != model.ProposalReverted {
		t.Fatalf("proposal status = %s, want reverted", status)
	}
	if o.RegressedOn != "cost" || o.NewCostPerSuccess == nil || *o.NewCostPerSuccess != 2.0 || o.PrevCostPerSuccess == nil || *o.PrevCostPerSuccess != 1.0 {
		t.Errorf("outcome = %+v", o)
	}
}

func TestABNoRevert(t *testing.T) {
	f := newABFixture(t)
	base := f.clock.Now()
	p := f.appliedProposal()
	f.fact(1, true, 1.0, base.Add(-2*time.Hour))
	f.fact(1, true, 1.0, base.Add(-1*time.Hour))
	// Fewer than K runs on the new generation: nothing happens yet.
	f.fact(2, true, 1.0, base.Add(1*time.Hour))
	f.srv.checkABReverts(actx(), ReflectionConfig{K: 2, Margin: 0.2})
	if status, _ := f.outcome(p); status != model.ProposalApplied {
		t.Fatalf("proposal status with < K runs = %s, want applied", status)
	}

	// K runs, no regression on either measure: still nothing.
	f.fact(2, true, 1.0, base.Add(2*time.Hour))
	f.srv.checkABReverts(actx(), ReflectionConfig{K: 2, Margin: 0.2})
	if status, _ := f.outcome(p); status != model.ProposalApplied {
		t.Fatalf("proposal status without regression = %s, want applied", status)
	}
	r, err := f.st.GetRoutine(actx(), "inventory")
	if err != nil {
		t.Fatal(err)
	}
	if r.Generation != 2 || r.Prompt != "new prompt" {
		t.Errorf("routine = generation %d prompt %q, want 2 / new prompt", r.Generation, r.Prompt)
	}
}
