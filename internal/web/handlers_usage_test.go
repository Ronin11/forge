package web

import (
	"context"
	"encoding/json"
	"math"
	"path/filepath"
	"testing"
	"time"

	"forge/internal/core/config"
	"forge/internal/core/engine"
	"forge/internal/core/store"
)

// Small copies of core/engine's budget test helpers (test helpers do not
// export across packages); the test itself exercises journalBudgetResets,
// which lives here.
var budgetCfg = config.BudgetConfig{FiveHourTarget: 0.9, SevenDayTarget: 0.9, FiveHourHardStop: 0.97, SevenDayHardStop: 0.97, ForecastPacing: true}

func bctx() context.Context { return context.Background() }

func near(a, b float64) bool { return math.Abs(a-b) < 1e-9 }

func sample(t time.Time, window string, u float64, resets time.Time) store.RateLimitSample {
	return store.RateLimitSample{Time: t, Window: window, Utilization: u, ResetsAt: resets}
}

func budgetStore(t *testing.T, clock func() time.Time) *store.Store {
	t.Helper()
	st, err := store.Open(bctx(), filepath.Join(t.TempDir(), "forge.sqlite3"), store.Options{Clock: clock})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := st.Close(); err != nil {
			t.Error(err)
		}
	})
	return st
}

func TestJournalBudgetResets(t *testing.T) {
	now := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	clock := func() time.Time { return now }
	st := budgetStore(t, clock)
	resetsA := now.Add(time.Hour)
	resetsB := now.Add(6 * time.Hour)
	err := st.Write(bctx(), func(tx *store.Tx) error {
		return tx.InsertSamples(bctx(), []store.RateLimitSample{sample(now.Add(-time.Hour), "five_hour", 0.8, resetsA)})
	})
	if err != nil {
		t.Fatal(err)
	}
	srv, err := NewServer(ServerOptions{Store: st, Policy: engine.NewBudgetPolicy(st, budgetCfg, clock)})
	if err != nil {
		t.Fatal(err)
	}
	incoming := []store.RateLimitSample{sample(now, "five_hour", 0.1, resetsB)}
	err = st.Write(bctx(), func(tx *store.Tx) error {
		if err := srv.journalBudgetResets(bctx(), tx, incoming); err != nil {
			return err
		}
		return tx.InsertSamples(bctx(), incoming)
	})
	if err != nil {
		t.Fatal(err)
	}
	entries, err := st.JournalForEntity(bctx(), store.EntityDaemon, "budget")
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 1 || entries[0].Kind != "budget.reset" {
		t.Fatalf("journal = %+v", entries)
	}
	var payload engine.BudgetReset
	if err := json.Unmarshal(entries[0].Payload, &payload); err != nil {
		t.Fatal(err)
	}
	if payload.Window != "five_hour" || !near(payload.Unspent, 0.9-0.8) || !payload.PrevResetsAt.Equal(resetsA) || !payload.NewResetsAt.Equal(resetsB) {
		t.Errorf("payload = %+v", payload)
	}
	// A first-ever seven_day sample has no prior boundary to journal.
	err = st.Write(bctx(), func(tx *store.Tx) error {
		first := []store.RateLimitSample{sample(now, "seven_day", 0.2, resetsB)}
		if err := srv.journalBudgetResets(bctx(), tx, first); err != nil {
			return err
		}
		return tx.InsertSamples(bctx(), first)
	})
	if err != nil {
		t.Fatal(err)
	}
	// Without the budget policy (engine.AdmitAll lacks the capability) nothing is journaled.
	plain, err := NewServer(ServerOptions{Store: st, Policy: engine.AdmitAll{}})
	if err != nil {
		t.Fatal(err)
	}
	err = st.Write(bctx(), func(tx *store.Tx) error {
		return plain.journalBudgetResets(bctx(), tx, []store.RateLimitSample{sample(now.Add(time.Minute), "five_hour", 0.05, now.Add(11*time.Hour))})
	})
	if err != nil {
		t.Fatal(err)
	}
	entries, err = st.JournalForEntity(bctx(), store.EntityDaemon, "budget")
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 1 {
		t.Errorf("journal grew unexpectedly: %+v", entries)
	}
}
