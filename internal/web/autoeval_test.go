package web

import (
	"context"
	"net/http"
	"os"
	"path/filepath"
	"sync"
	"testing"

	"forge/internal/core/store"
)

// fakeEval is an injected evalRunner: it records the modes it was asked to
// score and answers from a per-mode table, so tests exercise sweepAutoEval
// without ever running the real eval.Run harness.
type fakeEval struct {
	mu      sync.Mutex
	modes   []string
	answers map[string]struct {
		score float64
		ok    bool
	}
	block chan struct{} // when non-nil, each call waits on it before returning
}

func (f *fakeEval) run(ctx context.Context, mode string) (float64, []store.EvalCase, bool, error) {
	f.mu.Lock()
	f.modes = append(f.modes, mode)
	f.mu.Unlock()
	if f.block != nil {
		<-f.block
	}
	a := f.answers[mode]
	var cases []store.EvalCase
	if a.ok {
		cases = []store.EvalCase{{Mode: mode, CaseName: "golden-1", Pass: a.score >= 1, State: "succeeded"}}
	}
	return a.score, cases, a.ok, nil
}

func (f *fakeEval) calls() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.modes...)
}

// seedRoutineProposal creates routine "inventory" (mode run) and an ungraded
// routine proposal targeting it, the shape auto-eval scores.
func seedRoutineProposal(h *harness) store.Proposal {
	h.t.Helper()
	h.createRoutine("inventory")
	return createKindProposal(h, "routine", "routine:inventory", map[string]any{"prompt": "measured prompt"})
}

// NOTE: this grades a mode_prompt proposal. A routine proposal's eval mode is
// resolved from the stored row's Mode (autoeval.go proposalEvalMode), which is
// always empty now that routines are target-only — routine-kind proposals
// cannot resolve a mode until production reads it from the directive.
func TestAutoEvalRecordsScore(t *testing.T) {
	h := newHarness(t, transportUnix)
	p := createKindProposal(h, "mode_prompt", "mode:run", map[string]any{"content": "measured preamble"})

	fake := &fakeEval{answers: map[string]struct {
		score float64
		ok    bool
	}{"run": {0.75, true}}}
	h.srv.evalFn = fake.run

	h.srv.sweepAutoEval(context.Background())
	h.srv.autoEvalWG.Wait()

	if got := fake.calls(); len(got) != 1 || got[0] != "run" {
		t.Fatalf("evalFn calls = %v, want [run]", got)
	}
	var out store.Proposal
	h.call(http.MethodGet, "/api/v1/proposals/"+p.ID, nil, &out, http.StatusOK)
	if out.EvalScore == nil || *out.EvalScore != 0.75 {
		t.Fatalf("proposal eval score = %v, want 0.75", out.EvalScore)
	}
}

func TestAutoEvalSkipsModeWithoutCases(t *testing.T) {
	h := newHarness(t, transportUnix)
	// A mode_prompt proposal names the mode directly; the fake reports it has
	// no golden cases (ok=false), so the score must stay nil and no error.
	p := createKindProposal(h, "mode_prompt", "mode:run", map[string]any{"content": "new preamble"})

	fake := &fakeEval{answers: map[string]struct {
		score float64
		ok    bool
	}{}} // "run" absent → zero value {0, false}
	h.srv.evalFn = fake.run

	h.srv.sweepAutoEval(context.Background())
	h.srv.autoEvalWG.Wait()

	if got := fake.calls(); len(got) != 1 || got[0] != "run" {
		t.Fatalf("evalFn calls = %v, want [run]", got)
	}
	var out store.Proposal
	h.call(http.MethodGet, "/api/v1/proposals/"+p.ID, nil, &out, http.StatusOK)
	if out.EvalScore != nil {
		t.Fatalf("mode without cases got a score %v, want nil", *out.EvalScore)
	}
}

func TestAutoEvalInFlightGuard(t *testing.T) {
	h := newHarness(t, transportUnix)
	seedRoutineProposal(h)

	fake := &fakeEval{
		answers: map[string]struct {
			score float64
			ok    bool
		}{"run": {0.5, true}},
		block: make(chan struct{}),
	}
	h.srv.evalFn = fake.run

	// The first sweep launches the (blocking) eval; the second must see it in
	// flight and launch nothing, so the fake is invoked exactly once.
	h.srv.sweepAutoEval(context.Background())
	h.srv.sweepAutoEval(context.Background())
	close(fake.block)
	h.srv.autoEvalWG.Wait()

	if got := fake.calls(); len(got) != 1 {
		t.Fatalf("evalFn invoked %d times, want 1 (in-flight guard)", len(got))
	}
}

func TestAutoEvalIgnoresIneligible(t *testing.T) {
	h := newHarness(t, transportUnix)
	// A doc proposal is never gated, and an already-scored routine proposal is
	// done; auto-eval must touch neither.
	createKindProposal(h, "doc", "docs/plan.md", map[string]any{"content": "notes"})
	p := seedRoutineProposal(h)
	h.call(http.MethodPost, "/api/v1/proposals/"+p.ID+"/eval", map[string]float64{"score": 0.9}, nil, http.StatusOK)

	fake := &fakeEval{answers: map[string]struct {
		score float64
		ok    bool
	}{"run": {0.5, true}}}
	h.srv.evalFn = fake.run

	h.srv.sweepAutoEval(context.Background())
	h.srv.autoEvalWG.Wait()

	if got := fake.calls(); len(got) != 0 {
		t.Fatalf("evalFn invoked for ineligible proposals: %v", got)
	}
}

func TestEvalRootWalkUp(t *testing.T) {
	root := t.TempDir()
	mustMkdir(t, filepath.Join(root, "evals"))
	mustMkdir(t, filepath.Join(root, "testdata", "fixtures"))
	// A binary nested below the checkout root finds it by walking up.
	exe := filepath.Join(root, "cmd", "forge", "forge")
	mustMkdir(t, filepath.Dir(exe))
	if dir, ok := evalRoot(exe); !ok || dir != root {
		t.Fatalf("evalRoot(%q) = %q, %v; want %q, true", exe, dir, ok, root)
	}

	// A tree with neither evals/ nor fixtures yields (,false).
	bare := t.TempDir()
	if dir, ok := evalRoot(filepath.Join(bare, "forge")); ok {
		t.Fatalf("evalRoot in bare tree = %q, true; want _, false", dir)
	}
}

func mustMkdir(t *testing.T, dir string) {
	t.Helper()
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
}
