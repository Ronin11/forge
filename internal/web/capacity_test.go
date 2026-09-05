package web

import (
	"context"
	"encoding/json"
	"net/http"
	"testing"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/engine"
	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

// fakeCapacityPolicy is a SchedulerPolicy whose windows we position around
// the burn-down line.
type fakeCapacityPolicy struct {
	fiveUtil, fiveF   float64
	sevenUtil, sevenF float64
}

func (p *fakeCapacityPolicy) Decide(model.BudgetClass) (bool, string) { return true, "" }
func (p *fakeCapacityPolicy) Usage(context.Context) (engine.Usage, error) {
	return engine.Usage{
		FiveHour: engine.WindowUsage{Window: "five_hour", Utilization: p.fiveUtil, FractionElapsed: p.fiveF},
		SevenDay: engine.WindowUsage{Window: "seven_day", Utilization: p.sevenUtil, FractionElapsed: p.sevenF},
	}, nil
}
func (p *fakeCapacityPolicy) Config() config.BudgetConfig {
	return config.BudgetConfig{FiveHourTarget: 0.9, SevenDayTarget: 0.9}
}

// Surplus means every sampled window sits below target·f by the margin —
// prepaid capacity on track to expire unused.
func TestCapacitySurplus(t *testing.T) {
	h := newHarness(t, transportUnix)
	// Behind pace in both windows: f=0.8 puts the line at 0.72; util 0.2.
	h.srv.policy = &fakeCapacityPolicy{fiveUtil: 0.2, fiveF: 0.8, sevenUtil: 0.2, sevenF: 0.8}
	if !h.srv.capacitySurplus(context.Background(), surplusMargin) {
		t.Fatal("behind-pace windows should be surplus")
	}
	// One window on pace kills the surplus.
	h.srv.policy = &fakeCapacityPolicy{fiveUtil: 0.75, fiveF: 0.8, sevenUtil: 0.2, sevenF: 0.8}
	if h.srv.capacitySurplus(context.Background(), surplusMargin) {
		t.Fatal("an on-pace window is not surplus")
	}
	// No samples at all is never surplus.
	h.srv.policy = &fakeCapacityPolicy{fiveUtil: -1, sevenUtil: -1}
	if h.srv.capacitySurplus(context.Background(), surplusMargin) {
		t.Fatal("unsampled windows are not surplus")
	}
}

// Under surplus the backlog ceiling lifts: the third failed attempt reaches
// opus even though backlog_ceiling says sonnet.
func TestLadderCeilingLiftsOnSurplus(t *testing.T) {
	h := newRoutingHarness(t)
	h.registerRunners(testWorkerID)
	h.srv.routing.Ladder = []string{"haiku", "sonnet", "opus"}
	h.srv.routing.BacklogCeiling = "sonnet"
	h.srv.policy = &fakeCapacityPolicy{fiveUtil: 0.1, fiveF: 0.9, sevenUtil: 0.1, sevenF: 0.9}

	h.writeDirective("surgejob", "---\nmode: run\nmodel: haiku\n---\ndo {{repo}}\n")
	r := store.Routine{Name: "surgejob", Target: "directive:surgejob", Repositories: []string{"equitizr"},
		TimeoutSeconds: 300, RequireSandbox: true, BudgetClass: model.ClassBacklog}
	h.call(http.MethodPost, "/api/v1/routines", r, nil, http.StatusCreated)
	work := h.run("surgejob")
	target := work.Targets[0].ID

	c := h.mustClaim("sl1")
	for i, want := range []string{"sonnet", "opus"} {
		h.heartbeat(c, model.Preparing, 0)
		h.heartbeat(c, model.Running, 4321)
		cr := completeRequest(model.Failed, h.clock.now)
		cr.Verification = protocol.Verification{Level: 1, Passed: false}
		h.complete(c, cr)
		h.call(http.MethodPost, "/api/v1/targets/"+target+"/retry", nil, nil, http.StatusOK)
		c = h.mustClaim("sl" + string(rune('2'+i)))
		a, err := h.st.GetAttempt(context.Background(), c.AttemptID)
		if err != nil || a.ModelAlias != want {
			t.Fatalf("retry %d = %q, %v (want %s — ceiling lifted)", i+1, a.ModelAlias, err, want)
		}
	}
}

// The opportunist: on surplus, a live experiment short of min_runs gets a
// top-up run of its subject's trigger routine; the cooldown throttles.
func TestOpportunisticLearning(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutineWith("reflect-library", "reflect {{objective}}")
	openLiveExperiment(t, h.st, h.srv.promptLibrary(), "directive:reflect-library", "reflect-library",
		"---\nmode: run\nmodel: haiku\n---\nv {{objective}}\n")
	h.srv.refreshLiveExperiments(context.Background())
	h.srv.policy = &fakeCapacityPolicy{fiveUtil: 0.1, fiveF: 0.9, sevenUtil: 0.1, sevenF: 0.9}

	h.srv.opportunisticLearning(context.Background())
	rows, err := h.st.ListWork(context.Background(), 5)
	if err != nil {
		t.Fatal(err)
	}
	var fired *store.Work
	for i := range rows {
		if rows[i].SubmittedBy == "learning:opportunist" {
			fired = &rows[i]
		}
	}
	if fired == nil || fired.RoutineName != "reflect-library" {
		t.Fatalf("no opportunist work fired: %+v", rows)
	}
	var comp struct {
		Experiment string `json:"experiment"`
	}
	if err := json.Unmarshal(fired.Composition, &comp); err != nil || comp.Experiment == "" {
		t.Fatalf("top-up run not enrolled: %s (%v)", fired.Composition, err)
	}

	// Cooldown throttles the next firing (cancel the first work so the
	// open-work skip is not what stops it).
	h.call(http.MethodDelete, "/api/v1/work/"+fired.ID, nil, nil, http.StatusOK)
	h.srv.opportunisticLearning(context.Background())
	rows, _ = h.st.ListWork(context.Background(), 10)
	n := 0
	for _, w := range rows {
		if w.SubmittedBy == "learning:opportunist" {
			n++
		}
	}
	if n != 1 {
		t.Fatalf("cooldown ignored: %d opportunist works", n)
	}
	// Past the cooldown it fires again.
	h.clock.Advance(opportunistCooldown + time.Minute)
	h.srv.opportunisticLearning(context.Background())
	rows, _ = h.st.ListWork(context.Background(), 10)
	n = 0
	for _, w := range rows {
		if w.SubmittedBy == "learning:opportunist" {
			n++
		}
	}
	if n != 2 {
		t.Fatalf("post-cooldown fire = %d opportunist works, want 2", n)
	}
}
