package worker

import (
	"context"
	"log/slog"
	"testing"
	"time"

	"forge/internal/protocol"
)

// TestApplyGrant checks the worker actuation of a heartbeat-delivered budget
// grant: turns and seconds accumulate onto the attempt's effective budget, a
// budget.granted lifecycle event is emitted, and the paired nudge is attempted
// as a steer (dropped here, as there is no live process).
func TestApplyGrant(t *testing.T) {
	d := &fakeDaemon{}
	log := slog.New(slog.DiscardHandler)
	em := NewEmitter("att-1", d, log, func() time.Time { return time.Unix(0, 0) }, 1, 0)
	a := &attempt{emitter: em, log: log}

	a.applyHeartbeat(&protocol.HeartbeatResponse{
		GrantedBudget: &protocol.GrantedBudget{Dimension: "turns", Amount: 30},
		Nudge:         "Budget extended: 30 more turns granted. Keep going.",
	})
	a.applyHeartbeat(&protocol.HeartbeatResponse{
		GrantedBudget: &protocol.GrantedBudget{Dimension: "seconds", Amount: 120},
	})

	a.mu.Lock()
	turns, extra := a.grantedTurns, a.grantedTime
	a.mu.Unlock()
	if turns != 30 {
		t.Errorf("grantedTurns = %d, want 30", turns)
	}
	if extra != 120*time.Second {
		t.Errorf("grantedTime = %s, want 2m0s", extra)
	}

	// The grant surfaced as a lifecycle event, and the nudge was attempted (it
	// drops without a live steerable process, which is a visible event, never a
	// panic or silent loss). Lifecycle events carry their tag in Message.
	em.Flush(context.Background())
	d.mu.Lock()
	var granted, nudgeDropped int
	for _, e := range d.events {
		switch {
		case e.Kind == protocol.KindLifecycle && e.Message == "budget.granted":
			granted++
		case e.Kind == protocol.KindLifecycle && e.Message == "steer.dropped":
			nudgeDropped++
		}
	}
	d.mu.Unlock()
	if granted != 2 {
		t.Errorf("budget.granted events = %d, want 2", granted)
	}
	if nudgeDropped != 1 {
		t.Errorf("steer.dropped events = %d, want 1 (the nudge with no live process)", nudgeDropped)
	}
}
