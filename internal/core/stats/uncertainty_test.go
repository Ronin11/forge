package stats

import (
	"math"
	"testing"
)

func TestWilsonInterval(t *testing.T) {
	lo, hi := WilsonInterval(0, 0, 1.96)
	if lo != 0 || hi != 1 {
		t.Fatalf("n=0 = [%v, %v], want [0, 1]", lo, hi)
	}
	// The reflect-4 lesson: 2/3 looks like 67% but the interval is enormous.
	lo, hi = WilsonInterval(2, 3, 1.96)
	if lo > 0.30 || hi < 0.90 {
		t.Fatalf("2/3 = [%.2f, %.2f]: interval should be wide", lo, hi)
	}
	// At n=60 the same rate is a real finding.
	lo, hi = WilsonInterval(40, 60, 1.96)
	if lo < 0.5 || hi > 0.8 {
		t.Fatalf("40/60 = [%.2f, %.2f]: interval should be tight", lo, hi)
	}
	if lo2, _ := WilsonInterval(60, 60, 1.96); lo2 < 0.9 {
		t.Fatalf("60/60 lo = %.3f", lo2)
	}
}

func TestBetaSuperiority(t *testing.T) {
	// Symmetric evidence: a coin flip.
	if p := BetaSuperiority(3, 3, 3, 3); math.Abs(p-0.5) > 0.01 {
		t.Fatalf("symmetric = %.3f, want ~0.5", p)
	}
	// Strong separation: 9/1 vs 1/9.
	if p := BetaSuperiority(9, 1, 1, 9); p < 0.99 {
		t.Fatalf("9-1 vs 1-9 = %.3f, want > 0.99", p)
	}
	// Tiny n cannot be confident: 2/0 vs 0/2 is suggestive, not conclusive.
	if p := BetaSuperiority(2, 0, 0, 2); p < 0.8 || p > 0.98 {
		t.Fatalf("2-0 vs 0-2 = %.3f, want suggestive but not certain", p)
	}
	// Complement sums to one.
	a, b := BetaSuperiority(5, 2, 3, 4), BetaSuperiority(3, 4, 5, 2)
	if math.Abs(a+b-1) > 0.01 {
		t.Fatalf("P + complement = %.3f", a+b)
	}
}

func TestBetaVariance(t *testing.T) {
	// Zero data = maximal variance; more data always shrinks it.
	if v0, v10 := BetaVariance(0, 0), BetaVariance(5, 5); v0 <= v10 {
		t.Fatalf("variance did not shrink: %v vs %v", v0, v10)
	}
	if BetaVariance(0, 0) != 1.0/12 {
		t.Fatalf("uniform variance = %v, want 1/12", BetaVariance(0, 0))
	}
}
