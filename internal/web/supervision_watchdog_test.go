package web

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"testing"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/core/store"
)

// runningAttempt drives a claim to the running state so it appears in the sweep.
func (h *harness) runningAttempt(reqID string) *protocol.Claim {
	h.t.Helper()
	c := h.mustClaim(reqID)
	h.heartbeat(c, model.Preparing, 0)
	h.heartbeat(c, model.Running, 123)
	return c
}

// seedUsage inserts n assistant-turn usage metrics at the given time and
// refreshes the live tally, so RunningTurns == n and LastEventAt == at.
func (h *harness) seedUsage(attemptID string, n int, at time.Time) {
	h.t.Helper()
	evs := make([]protocol.Event, n)
	for i := 0; i < n; i++ {
		attrs, err := json.Marshal(map[string]any{"message_id": fmt.Sprintf("m%d", i), "input_tokens": 10, "output_tokens": 5})
		if err != nil {
			h.t.Fatal(err)
		}
		evs[i] = protocol.Event{Seq: i + 1, Time: at, Kind: protocol.KindMetric, Name: "usage", Message: "usage", Attrs: attrs}
	}
	err := h.st.Write(context.Background(), func(tx *store.Tx) error {
		if _, err := tx.InsertEvents(context.Background(), attemptID, protocol.SourceWorker, evs); err != nil {
			return err
		}
		return tx.RecomputeAttemptProgress(context.Background(), attemptID)
	})
	if err != nil {
		h.t.Fatal(err)
	}
}

func (h *harness) journalKinds(entityID string) map[string]int {
	h.t.Helper()
	entries, err := h.st.JournalForEntity(context.Background(), store.EntityAttempt, entityID)
	if err != nil {
		h.t.Fatal(err)
	}
	out := map[string]int{}
	for _, e := range entries {
		out[e.Kind]++
	}
	return out
}

// TestSweepShadowDoesNotCancel is the whole point of shadow mode: a kill
// decision (here an over-ceiling one) journals attempt.would_reap but cancels
// nothing while enforce_kill is false.
func TestSweepShadowDoesNotCancel(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	work := h.run("inventory")
	c := h.runningAttempt("req-1")

	h.srv.supervisionCfg = config.SupervisionConfig{HardCeilingTurns: 5, SoftTurns: 4, SilenceMinutes: 5, SpinWindowTurns: 25, MaxAutoExtensions: 3, EnforceKill: false}
	// 6 turns at the current clock (recent → not silent), past the ceiling of 5.
	h.seedUsage(c.AttemptID, 6, h.clock.Now())

	h.srv.sweepSupervision(context.Background())

	if k := h.journalKinds(c.AttemptID); k["attempt.would_reap"] != 1 || k["attempt.reaped"] != 0 {
		t.Errorf("shadow journals = %+v, want one would_reap and no reaped", k)
	}
	if tg := h.target(work.Targets[0].ID); tg.CancelRequested {
		t.Error("shadow mode must not request cancel")
	}
}

// TestSweepEnforceReaps is the mirror: with enforce_kill true the same decision
// cancels the attempt through the existing path and journals attempt.reaped.
func TestSweepEnforceReaps(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	work := h.run("inventory")
	c := h.runningAttempt("req-1")

	h.srv.supervisionCfg = config.SupervisionConfig{HardCeilingTurns: 5, SoftTurns: 4, SilenceMinutes: 5, SpinWindowTurns: 25, MaxAutoExtensions: 3, EnforceKill: true}
	h.seedUsage(c.AttemptID, 6, h.clock.Now())

	h.srv.sweepSupervision(context.Background())

	if k := h.journalKinds(c.AttemptID); k["attempt.reaped"] != 1 || k["attempt.would_reap"] != 0 {
		t.Errorf("enforce journals = %+v, want one reaped and no would_reap", k)
	}
	if tg := h.target(work.Targets[0].ID); !tg.CancelRequested {
		t.Error("enforce mode must request cancel")
	}
}

// TestSweepSilenceEscalatesShadow drives the ambiguous-silence path through the
// injected decider and confirms shadow mode still cancels nothing.
func TestSweepSilenceEscalatesShadow(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	work := h.run("inventory")
	c := h.runningAttempt("req-1")

	var calls int
	h.srv.modelCall = func(_ context.Context, _, user, _ string) (string, error) {
		calls++
		if !strings.Contains(user, "Silent for") {
			t.Errorf("decider prompt missing silence evidence: %q", user)
		}
		return `{"action":"kill","rationale":"hung"}`, nil
	}
	h.srv.supervisionCfg = testSupervisionCfg()
	// One turn, then advance the clock past the silence window so the sweep sees a hang.
	h.seedUsage(c.AttemptID, 1, h.clock.Now())
	h.clock.Advance(10 * time.Minute)

	h.srv.sweepSupervision(context.Background())

	if calls != 1 {
		t.Errorf("decider calls = %d, want 1", calls)
	}
	if k := h.journalKinds(c.AttemptID); k["attempt.would_reap"] != 1 {
		t.Errorf("silence shadow journals = %+v, want one would_reap", k)
	}
	if tg := h.target(work.Targets[0].ID); tg.CancelRequested {
		t.Error("shadow mode must not cancel on a silence escalation")
	}
}

// A cliff-triggered adjudication that comes back "continue" on a progressing
// attempt converts to a bounded turns extension: at the cap, continue is only
// actionable as turns — three continue verdicts preceded a budget_cliff death
// on 2026-09-06, with the supervisor's approval of the progress on record.
func TestSweepCliffContinueBecomesExtension(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	h.run("inventory")
	c := h.runningAttempt("req-1")

	h.srv.modelCall = func(_ context.Context, _, _, _ string) (string, error) {
		return `{"action":"continue","rationale":"healthy, finishing up"}`, nil
	}
	h.srv.supervisionCfg = config.SupervisionConfig{HardCeilingTurns: 20, SoftTurns: 10, SilenceMinutes: 60, SpinWindowTurns: 25, MaxAutoExtensions: 3}
	h.seedUsage(c.AttemptID, 7, h.clock.Now()) // 7 >= 20-15: inside the cliff margin
	// One file-mutating span: ArtifactGrowth > 0 with no ledger → GrewSinceLast.
	attrs, err := json.Marshal(map[string]any{})
	if err != nil {
		t.Fatal(err)
	}
	err = h.st.Write(context.Background(), func(tx *store.Tx) error {
		_, err := tx.InsertEvents(context.Background(), c.AttemptID, protocol.SourceWorker,
			[]protocol.Event{{Seq: 100, Time: h.clock.Now(), Kind: protocol.KindSpanStart, SpanID: "s1", Name: "Write", Message: "Write", Attrs: attrs}})
		return err
	})
	if err != nil {
		t.Fatal(err)
	}

	h.srv.sweepSupervision(context.Background())

	k := h.journalKinds(c.AttemptID)
	if k["attempt.budget_granted"] != 1 {
		t.Fatalf("journals = %+v, want one attempt.budget_granted", k)
	}
	if k["attempt.would_reap"] != 0 || k["attempt.reaped"] != 0 {
		t.Errorf("no reap expected: %+v", k)
	}
	ledger, err := h.st.BudgetLedger(context.Background(), c.AttemptID)
	if err != nil {
		t.Fatal(err)
	}
	last := ledger[len(ledger)-1]
	if last.Decision != store.BudgetExtend || last.GrantedAmount <= 0 || last.GrantedAmount > 10 {
		t.Errorf("ledger = %+v, want extend with 0 < amount <= soft slot", last)
	}
}

// The same cliff with a flailing attempt (no artifact growth) stays a
// continue/kill call — no free extension for spinning.
func TestSweepCliffNoGrowthNoExtension(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	h.run("inventory")
	c := h.runningAttempt("req-1")

	h.srv.modelCall = func(_ context.Context, _, _, _ string) (string, error) {
		return `{"action":"continue","rationale":"unsure"}`, nil
	}
	h.srv.supervisionCfg = config.SupervisionConfig{HardCeilingTurns: 20, SoftTurns: 10, SilenceMinutes: 60, SpinWindowTurns: 25, MaxAutoExtensions: 3}
	h.seedUsage(c.AttemptID, 7, h.clock.Now())

	h.srv.sweepSupervision(context.Background())

	if k := h.journalKinds(c.AttemptID); k["attempt.budget_granted"] != 0 {
		t.Errorf("journals = %+v, want no grant without growth", k)
	}
}
