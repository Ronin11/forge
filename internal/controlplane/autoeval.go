package controlplane

import (
	"context"
	"fmt"
	"os"
	"path/filepath"

	"forge/internal/core/model"
	"forge/internal/core/store"
	"forge/internal/eval"
)

// evalRunner runs the golden eval for a mode and returns its summary score in
// [0,1]. ok is false — not an error — when auto-eval cannot score the mode:
// the daemon binary is not in its checkout (no evals/ found) or the mode has
// no golden cases. In that case the proposal keeps its nil eval score and the
// human uses the force override (handlers_proposals.go). It is a Server field
// so tests inject a fake and never spin the real eval harness.
type evalRunner func(ctx context.Context, mode string) (score float64, ok bool, err error)

// sweepAutoEval satisfies the eval gate for the daemon itself: for each
// ungraded routine/mode_prompt proposal it runs that mode's golden eval and
// records the score, so a prompt change can be approved without a human `forge
// eval` step. It launches at most one eval per tick and never two at once
// (the process-wide slot and the per-proposal in-flight guard), because
// eval.Run shells out per case and takes seconds; the eval runs in a goroutine
// owned by RunSweeper so a slow eval never stalls the lease sweep.
func (s *Server) sweepAutoEval(ctx context.Context) {
	proposals, err := s.store.ListProposals(ctx, model.ProposalProposed)
	if err != nil {
		s.log.ErrorContext(ctx, "auto-eval: list proposals", "error", err)
		return
	}
	for i := range proposals {
		p := &proposals[i]
		if p.EvalScore != nil {
			continue
		}
		if p.Kind != model.ProposalRoutine && p.Kind != model.ProposalModePrompt {
			continue
		}
		mode, err := s.proposalEvalMode(ctx, p)
		if err != nil {
			s.log.WarnContext(ctx, "auto-eval: resolve mode", "proposal_id", p.ID, "error", err)
			continue
		}
		if !s.beginEval(p.ID) {
			continue // an eval for this proposal is already in flight
		}
		// One eval at a time, process-wide: if the slot is taken, drop the
		// in-flight mark and leave this proposal for a later tick.
		select {
		case s.autoEvalSem <- struct{}{}:
		default:
			s.endEval(p.ID)
			return
		}
		s.autoEvalWG.Add(1)
		go s.runOneAutoEval(ctx, p.ID, mode)
		return // at most one new eval launched per tick
	}
}

// runOneAutoEval runs one proposal's eval and records the score. It releases
// the slot and the in-flight mark on every path so the next tick can proceed.
// A missing-cases result (ok=false) leaves the score nil for the override —
// the next tick's cheap dir check skips the proposal again, so there is no
// retry storm.
func (s *Server) runOneAutoEval(ctx context.Context, id, mode string) {
	defer s.autoEvalWG.Done()
	defer func() { <-s.autoEvalSem }()
	defer s.endEval(id)

	score, ok, err := s.evalFn(ctx, mode)
	if err != nil {
		s.log.WarnContext(ctx, "auto-eval failed", "proposal_id", id, "mode", mode, "error", err)
		return
	}
	if !ok {
		s.log.DebugContext(ctx, "auto-eval skipped: no golden cases for mode", "proposal_id", id, "mode", mode)
		return
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error {
		_, werr := tx.SetProposalEvalScore(ctx, id, score)
		return werr
	}); err != nil {
		s.log.WarnContext(ctx, "auto-eval: record score", "proposal_id", id, "score", score, "error", err)
		return
	}
	s.log.InfoContext(ctx, "auto-eval scored proposal", "proposal_id", id, "mode", mode, "score", score)
}

// proposalEvalMode resolves the mode whose golden cases grade a proposal: a
// routine proposal evaluates the routine's mode; a mode_prompt proposal names
// the mode directly. The target may carry the kind prefix or a bare name,
// matching the apply engine (apply.go targetName).
func (s *Server) proposalEvalMode(ctx context.Context, p *store.Proposal) (string, error) {
	switch p.Kind {
	case model.ProposalRoutine:
		name, err := targetName(p.Target, "routine:")
		if err != nil {
			return "", err
		}
		r, err := s.store.GetRoutine(ctx, name)
		if err != nil {
			return "", err
		}
		return r.Mode, nil
	case model.ProposalModePrompt:
		return targetName(p.Target, "mode:")
	default:
		return "", fmt.Errorf("proposal %s: kind %q is not auto-evaluated", model.ShortID(p.ID), p.Kind)
	}
}

// beginEval marks a proposal's eval in flight, returning false if one is
// already running for it so a slow eval is never launched twice across ticks.
func (s *Server) beginEval(id string) bool {
	s.inflightMu.Lock()
	defer s.inflightMu.Unlock()
	if s.inflightEval[id] {
		return false
	}
	s.inflightEval[id] = true
	return true
}

// endEval clears the in-flight mark once an eval finishes.
func (s *Server) endEval(id string) {
	s.inflightMu.Lock()
	defer s.inflightMu.Unlock()
	delete(s.inflightEval, id)
}

// runEval is the real evalRunner: it runs the golden cases for a mode through
// eval.Run (fake-claude fixtures, an isolated temp home per case) and returns
// the summary score. ok is false — not an error — when auto-eval is disabled
// (the binary is not in its checkout) or the mode has no golden cases, so the
// sweep leaves the score nil for the override rather than erroring or looping.
func (s *Server) runEval(ctx context.Context, mode string) (float64, bool, error) {
	root, ok := s.evalRootCached(ctx)
	if !ok {
		return 0, false, nil
	}
	if !modeHasCases(filepath.Join(root, "evals", mode)) {
		return 0, false, nil
	}
	workDir, err := os.MkdirTemp("", "forge-autoeval-")
	if err != nil {
		return 0, false, fmt.Errorf("auto-eval work dir: %w", err)
	}
	defer func() {
		if rerr := os.RemoveAll(workDir); rerr != nil {
			s.log.WarnContext(ctx, "auto-eval: clean work dir", "dir", workDir, "error", rerr)
		}
	}()
	rep, err := eval.Run(ctx, eval.Options{
		Mode:        mode,
		CasesDir:    filepath.Join(root, "evals"),
		FixturesDir: filepath.Join(root, "testdata", "fixtures"),
		ForgeBin:    s.exe,
		WorkDir:     workDir,
		Logger:      s.log,
	})
	if err != nil {
		return 0, false, fmt.Errorf("run eval for mode %s: %w", mode, err)
	}
	return rep.Score, true, nil
}

// evalRootCached returns the checkout root for auto-eval, discovered once from
// the daemon binary and cached under evalRootMu. The first miss logs once at
// info so an operator sees why auto-eval is off; later calls are silent.
func (s *Server) evalRootCached(ctx context.Context) (string, bool) {
	s.evalRootMu.Lock()
	defer s.evalRootMu.Unlock()
	if !s.evalRootDone {
		s.evalRootDir, s.evalRootOK = evalRoot(s.exe)
		s.evalRootDone = true
		if !s.evalRootOK {
			s.log.InfoContext(ctx, "auto-eval disabled: no evals/ and testdata/fixtures/ above the daemon binary; approvals use force", "exe", s.exe)
		}
	}
	return s.evalRootDir, s.evalRootOK
}

// evalRoot walks up from the daemon binary to the first directory holding both
// evals/ and testdata/fixtures/ — the golden cases and the fake-claude
// fixtures the auto-eval replays. It mirrors findRepoPluginDirFrom: a daemon
// built inside its checkout finds them; one installed elsewhere returns
// (,false) and auto-eval stays disabled. Only a few non-blocking stats, so no
// context (STYLE.md §3).
func evalRoot(exe string) (string, bool) {
	dir := filepath.Dir(exe)
	for {
		_, evalsErr := os.Stat(filepath.Join(dir, "evals"))
		_, fixturesErr := os.Stat(filepath.Join(dir, "testdata", "fixtures"))
		if evalsErr == nil && fixturesErr == nil {
			return dir, true
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return "", false
		}
		dir = parent
	}
}

// modeHasCases reports whether an evals/<mode> directory holds at least one
// case subdirectory with an eval.toml — a cheap check so a mode with no golden
// cases is skipped without shelling out to eval.Run.
func modeHasCases(modeDir string) bool {
	entries, err := os.ReadDir(modeDir)
	if err != nil {
		return false
	}
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		if _, err := os.Stat(filepath.Join(modeDir, e.Name(), "eval.toml")); err == nil {
			return true
		}
	}
	return false
}
