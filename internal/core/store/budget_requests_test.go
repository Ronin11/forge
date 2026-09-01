package store

import (
	"encoding/json"
	"fmt"
	"testing"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
)

// spanEvent builds a tool_use span_start event the way the worker's parser
// stamps them: the tool name plus an input_summary in attrs.
func spanEvent(t *testing.T, seq int, at time.Time, tool, summary string) protocol.Event {
	t.Helper()
	attrs, err := json.Marshal(map[string]any{"tool": tool, "input_summary": summary})
	if err != nil {
		t.Fatal(err)
	}
	return protocol.Event{Seq: seq, Time: at, Kind: protocol.KindSpanStart, Name: tool, SpanID: fmt.Sprintf("span-%d", seq), Attrs: attrs}
}

func TestBudgetLedgerAppendAndOrdering(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassInteractive)
	a := f.claim(target, "req-1")

	// Three requests appended in order get seq 1,2,3, and the snapshot round-trips.
	for i := 1; i <= 3; i++ {
		snap := must(json.Marshal(map[string]any{"artifact_growth": i * 2}))
		f.write(func(tx *Tx) error {
			r, err := tx.AppendBudgetRequest(ctx(), BudgetRequest{
				AttemptID: a.ID, Dimension: BudgetTurns, Amount: float64(10 * i), Reason: fmt.Sprintf("ask %d", i),
				Decision: BudgetExtend, GrantedAmount: float64(5 * i), ProgressSnapshot: snap, DecidedBy: "policy", Rationale: "ok",
			})
			if err != nil {
				return err
			}
			if r.Seq != i {
				t.Errorf("append %d: seq = %d, want %d", i, r.Seq, i)
			}
			return nil
		})
	}

	ledger := must(f.s.BudgetLedger(ctx(), a.ID))
	if len(ledger) != 3 {
		t.Fatalf("ledger len = %d, want 3", len(ledger))
	}
	for i, r := range ledger {
		if r.Seq != i+1 {
			t.Errorf("row %d seq = %d", i, r.Seq)
		}
		if snapshotGrowthFor(t, r) != (i+1)*2 {
			t.Errorf("row %d snapshot growth = %d, want %d", i, snapshotGrowthFor(t, r), (i+1)*2)
		}
	}
}

func snapshotGrowthFor(t *testing.T, r BudgetRequest) int {
	t.Helper()
	var s struct {
		ArtifactGrowth int `json:"artifact_growth"`
	}
	if err := json.Unmarshal(r.ProgressSnapshot, &s); err != nil {
		t.Fatal(err)
	}
	return s.ArtifactGrowth
}

func TestArtifactGrowthAndDominance(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassInteractive)
	a := f.claim(target, "req-1")
	f.run(a.ID, "lease-req-1")

	base := f.now.Add(time.Minute)
	// Five reads of the same file (one dominant signature) plus two edits.
	evs := []protocol.Event{
		spanEvent(t, 1, base, "Read", "/x/foo.go"),
		spanEvent(t, 2, base.Add(1*time.Second), "Read", "/x/foo.go"),
		spanEvent(t, 3, base.Add(2*time.Second), "Read", "/x/foo.go"),
		spanEvent(t, 4, base.Add(3*time.Second), "Read", "/x/foo.go"),
		spanEvent(t, 5, base.Add(4*time.Second), "Read", "/x/foo.go"),
		spanEvent(t, 6, base.Add(5*time.Second), "Edit", "/x/foo.go"),
		spanEvent(t, 7, base.Add(6*time.Second), "Write", "/x/bar.go"),
	}
	f.write(func(tx *Tx) error {
		_, err := tx.InsertEvents(ctx(), a.ID, protocol.SourceWorker, evs)
		return err
	})

	if g := must(f.s.ArtifactGrowth(ctx(), a.ID)); g != 2 {
		t.Errorf("artifact growth = %d, want 2 (Edit+Write)", g)
	}
	dom, top, samples, err := f.s.ToolDominance(ctx(), a.ID, 25)
	if err != nil {
		t.Fatal(err)
	}
	if samples != 7 {
		t.Errorf("samples = %d, want 7", samples)
	}
	// The repeated Read of the same path dominates 5/7.
	if dom < 0.7 || dom > 0.72 {
		t.Errorf("dominance = %.3f, want ~0.714", dom)
	}
	if wantTop := toolSignature("Read", "/x/foo.go"); top != wantTop {
		t.Errorf("top signature = %q, want %q", top, wantTop)
	}
}

func TestTakeBudgetGrants(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassInteractive)
	a := f.claim(target, "req-1")

	f.write(func(tx *Tx) error {
		if err := tx.Journal(ctx(), "attempt.budget_granted", EntityAttempt, a.ID, map[string]any{"dimension": "turns", "granted_amount": 30.0, "nudge": "keep going"}); err != nil {
			return err
		}
		return tx.Journal(ctx(), "attempt.budget_granted", EntityAttempt, a.ID, map[string]any{"dimension": "seconds", "granted_amount": 120.0, "nudge": "more time"})
	})

	var grants []GrantedBudget
	f.write(func(tx *Tx) error {
		var err error
		grants, err = tx.TakeBudgetGrants(ctx(), a.ID)
		return err
	})
	if len(grants) != 2 {
		t.Fatalf("grants = %d, want 2", len(grants))
	}
	if grants[0].Dimension != "turns" || grants[0].Amount != 30 || grants[0].Nudge != "keep going" {
		t.Errorf("grant 0 = %+v", grants[0])
	}
	if grants[1].Dimension != "seconds" || grants[1].Amount != 120 {
		t.Errorf("grant 1 = %+v", grants[1])
	}
	// A second take sees the watermark and returns nothing.
	f.write(func(tx *Tx) error {
		again, err := tx.TakeBudgetGrants(ctx(), a.ID)
		if err != nil {
			return err
		}
		if len(again) != 0 {
			t.Errorf("second take = %d, want 0", len(again))
		}
		return nil
	})
}

func TestRunningAttemptsAndCancel(t *testing.T) {
	f := newFixture(t)
	_, target := f.newWork(model.ClassInteractive)
	a := f.claim(target, "req-1")
	f.run(a.ID, "lease-req-1")

	running := must(f.s.RunningAttempts(ctx()))
	if len(running) != 1 || running[0].ID != a.ID || running[0].TargetID != target.ID {
		t.Fatalf("running attempts = %+v, want the one running attempt", running)
	}

	f.write(func(tx *Tx) error {
		ok, err := tx.RequestTargetCancel(ctx(), target.ID, "supervisor")
		if err != nil {
			return err
		}
		if !ok {
			t.Error("cancel did not flag a running target")
		}
		return nil
	})
	// cancel_requested is set and journaled.
	var cancel int
	if err := f.s.queryRow(ctx(), `SELECT cancel_requested FROM targets WHERE id = ?`, target.ID).Scan(&cancel); err != nil || cancel != 1 {
		t.Errorf("cancel_requested = %d, %v", cancel, err)
	}
	f.write(func(tx *Tx) error {
		has, err := tx.HasJournal(ctx(), EntityTarget, target.ID, "target.cancel_requested")
		if err != nil {
			return err
		}
		if !has {
			t.Error("no target.cancel_requested journal row")
		}
		return nil
	})
}
