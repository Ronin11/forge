package stats

import "math"

// Objective uncertainty (DESIGN: "reduce uncertainty → better learning").
// Every success rate in forge is a Bernoulli estimate; these are the three
// operations the rest of the system builds on: an interval that widens
// honestly at small n (display + insufficient-evidence gates), the posterior
// probability one rate beats another (experiment decisions), and the
// posterior variance (uncertainty-sampling allocation).

// WilsonInterval is the score interval for successes/n at z (1.96 ≈ 95%).
// n = 0 returns the maximally ignorant [0, 1].
func WilsonInterval(successes, n int, z float64) (lo, hi float64) {
	if n <= 0 {
		return 0, 1
	}
	p := float64(successes) / float64(n)
	nf := float64(n)
	denom := 1 + z*z/nf
	center := p + z*z/(2*nf)
	spread := z * math.Sqrt(p*(1-p)/nf+z*z/(4*nf*nf))
	lo = (center - spread) / denom
	hi = (center + spread) / denom
	return math.Max(lo, 0), math.Min(hi, 1)
}

// betaGrid is the resolution of the numeric posterior computations; 2001
// points keeps BetaSuperiority within ~1e-3 of exact for the counts forge
// sees (single digits to hundreds).
const betaGrid = 2001

// betaLogPDF is log of the Beta(a, b) density at x.
func betaLogPDF(x, a, b float64) float64 {
	if x <= 0 || x >= 1 {
		return math.Inf(-1)
	}
	la, _ := math.Lgamma(a)
	lb, _ := math.Lgamma(b)
	lab, _ := math.Lgamma(a + b)
	return lab - la - lb + (a-1)*math.Log(x) + (b-1)*math.Log(1-x)
}

// BetaSuperiority is P(X > Y) for X ~ Beta(1+xs, 1+xf), Y ~ Beta(1+ys, 1+yf)
// — the probability that the arm with xs successes and xf failures is truly
// better than the arm with ys/yf, under uniform priors. Computed by grid
// integration: Σ pdf_X(t) · CDF_Y(t) · dt.
func BetaSuperiority(xs, xf, ys, yf int) float64 {
	ax, bx := float64(xs)+1, float64(xf)+1
	ay, by := float64(ys)+1, float64(yf)+1
	dt := 1.0 / float64(betaGrid)
	var cdfY, p float64
	for i := 0; i < betaGrid; i++ {
		t := (float64(i) + 0.5) * dt
		cdfY += math.Exp(betaLogPDF(t, ay, by)) * dt
		p += math.Exp(betaLogPDF(t, ax, bx)) * math.Min(cdfY, 1) * dt
	}
	return math.Min(math.Max(p, 0), 1)
}

// BetaVariance is the posterior variance of Beta(1+successes, 1+failures) —
// what uncertainty sampling allocates the next run by.
func BetaVariance(successes, failures int) float64 {
	a, b := float64(successes)+1, float64(failures)+1
	return a * b / ((a + b) * (a + b) * (a + b + 1))
}

// insufficientWidth: an interval wider than this carries too little
// information to diagnose from — roughly n < 8 at mid rates.
const insufficientWidth = 0.55
