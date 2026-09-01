package doctor

import "testing"

func TestPricingDrift(t *testing.T) {
	// Too few samples: informational ok, never a warn.
	if c := pricing(DaemonInput{PricingPairs: []PricingPair{{Notional: 1, Reported: 1}}}); c.Status != StatusOK {
		t.Errorf("one sample should be ok, got %+v", c)
	}
	// Aligned prices: ok.
	aligned := []PricingPair{{Notional: 1.00, Reported: 1.00}, {Notional: 2.00, Reported: 2.02}, {Notional: 0.50, Reported: 0.49}}
	if c := pricing(DaemonInput{PricingPairs: aligned}); c.Status != StatusOK {
		t.Errorf("aligned prices should be ok, got %+v", c)
	}
	// A stale table: notional is ~2x reported → warn.
	stale := []PricingPair{{Notional: 2.0, Reported: 1.0}, {Notional: 4.0, Reported: 2.0}, {Notional: 1.0, Reported: 0.5}}
	c := pricing(DaemonInput{PricingPairs: stale})
	if c.Status != StatusWarn {
		t.Fatalf("stale prices should warn, got %+v", c)
	}
	if c.Hint == "" {
		t.Error("a pricing warn should hint at refreshing the table")
	}
}
