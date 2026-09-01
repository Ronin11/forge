// Package integrator drives the merge queue (DESIGN.md §20): one serial loop
// that takes queued_for_merge Targets, rebases their task branches onto the
// repository's integration branch in a scratch clone, re-runs the declared
// checks on the rebased result, and pushes the fast-forward through the
// push-policy gate (constitution 10). It runs inside the daemon process —
// never in a sandbox — and touches a registered checkout only with read-only
// operations (clone, fetch, remote get-url; constitution 1).
//
// V0 scope, recorded in NOTES.md: the DESIGN §4.1 worker-side merge claim is
// not implemented — the daemon does the git work itself, serial per tick, so
// no lease is held and a crash mid-merge is recovered by requeueing merging
// Targets at the next tick. Conflicts land in the human queue with the
// scratch clone retained; the automatic integrate-mode attempt is not
// spawned yet.
package integrator

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/store"
	"forge/internal/worker"
)

// Tuning of the loop. The per-command git timeout is generous because a
// clone of a large checkout is one command.
const (
	defaultInterval   = 15 * time.Second
	gitTimeout        = 5 * time.Minute
	maxRebaseRounds   = 100 // conflicted commits resolved per rebase, a safety cap
	defaultMaxRebase  = 3
	scratchDirName    = "integrator"
	mergirafBinary    = "mergiraf"
	outcomeMerged     = "merged"
	outcomeConflict   = "conflict"
	outcomeChecksFail = "checks_failed"
)

// Config is what the daemon hands the integrator from its own config.
type Config struct {
	Home              string // <forge home>; scratch clones live under Home/integrator
	MaxRebaseAttempts int    // [integration] max_rebase_attempts; 0 → 3
	Interval          time.Duration
}

// Integrator owns the merge queue for every repository the store knows.
type Integrator struct {
	st    *store.Store
	log   *slog.Logger
	clock func() time.Time
	cfg   Config
	git   worker.Git
}

// New builds the integrator; nothing runs until Run.
func New(st *store.Store, log *slog.Logger, clock func() time.Time, cfg Config) *Integrator {
	if log == nil {
		log = slog.New(slog.DiscardHandler)
	}
	if clock == nil {
		clock = time.Now
	}
	if cfg.MaxRebaseAttempts <= 0 {
		cfg.MaxRebaseAttempts = defaultMaxRebase
	}
	if cfg.Interval <= 0 {
		cfg.Interval = defaultInterval
	}
	return &Integrator{st: st, log: log, clock: clock, cfg: cfg, git: worker.Git{Timeout: gitTimeout, Env: gitEnv()}}
}

// gitEnv is every git option the integrator applies — through the
// environment, never a .git/config write (STYLE.md §10): zdiff3 markers for
// mergiraf, rerere, a committer identity for rebased commits, and quiet
// non-interactive editors.
func gitEnv() []string {
	env := worker.GitConfigEnv(map[string]string{
		"merge.conflictstyle": "zdiff3",
		"rerere.enabled":      "true",
		"advice.detachedHead": "false",
		"commit.gpgsign":      "false",
		"user.name":           "forge-integrator",
		"user.email":          "forge-integrator@localhost",
	})
	return append(env, "GIT_EDITOR=true", "GIT_SEQUENCE_EDITOR=true")
}

// Run ticks until ctx ends. One goroutine: repositories are processed
// serially, which is the contract (one integrator per repository, and this
// process is the one integrator for all of them).
func (i *Integrator) Run(ctx context.Context) {
	t := time.NewTicker(i.cfg.Interval)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			i.Tick(ctx)
		}
	}
}

// Tick is one pass: recover Targets a crash left in merging, then process
// the oldest queued_for_merge Target of each repository. Exported so tests
// drive the loop deterministically.
func (i *Integrator) Tick(ctx context.Context) {
	i.recoverMerging(ctx)
	queued, err := i.st.TargetsInState(ctx, model.QueuedForMerge)
	if err != nil {
		i.log.ErrorContext(ctx, "read merge queue", "error", err)
		return
	}
	seen := map[string]bool{}
	for _, t := range queued {
		if seen[t.Repository] {
			continue // oldest first; the rest of this repository waits for the next tick
		}
		seen[t.Repository] = true
		i.process(ctx, t)
	}
}

// recoverMerging requeues merging Targets that hold no lease: the daemon-side
// integrator is the only thing that sets merging without a lease, and it is
// synchronous — so any such row at tick start is a leftover from a crash.
// A leased merging Target (the worker-side merge claim, when it exists) is
// the sweeper's to expire, not ours.
func (i *Integrator) recoverMerging(ctx context.Context) {
	merging, err := i.st.TargetsInState(ctx, model.Merging)
	if err != nil {
		i.log.ErrorContext(ctx, "read merging targets", "error", err)
		return
	}
	for _, t := range merging {
		if !t.LeaseExpiresAt.IsZero() {
			continue
		}
		err := i.st.Write(ctx, func(tx *store.Tx) error {
			_, terr := tx.Transition(ctx, t.ID, model.QueuedForMerge, store.TransitionOptions{Actor: "integrator"})
			return terr
		})
		if err != nil {
			i.log.ErrorContext(ctx, "requeue stale merging target", "target_id", t.ID, "error", err)
			continue
		}
		i.log.WarnContext(ctx, "merging target from a previous run requeued", "target_id", t.ID)
	}
}

// mergeOutcome is what one merge attempt decided.
type mergeOutcome struct {
	outcome    string // outcomeMerged | outcomeConflict | outcomeChecksFail; "" = infrastructure retry
	reason     string
	before     string // integration branch head before the push
	after      string // pushed head
	conflicted []string
	scratch    string // retained on conflict
	touched    []string
	checkName  string // first failing check, for the unverified reason
}

// process runs one Target through merging and lands the outcome. Every store
// change is its own transaction; the git work happens between them, so a
// crash leaves at worst a merging row the next tick requeues.
func (i *Integrator) process(ctx context.Context, t store.Target) {
	log := i.log.With("target_id", t.ID, "work_id", t.WorkID, "repository", t.Repository)
	w, err := i.st.GetWork(ctx, t.WorkID)
	if err != nil {
		log.ErrorContext(ctx, "read work", "error", err)
		return
	}
	if !w.Integrate {
		log.ErrorContext(ctx, "queued_for_merge target of a non-integrating work; leaving it for a human")
		return
	}
	if t.CancelRequested {
		i.land(ctx, t, model.Cancelled, store.TransitionOptions{Reason: model.ReasonCancelled, Actor: "integrator"})
		return
	}
	a, err := i.st.AttemptForTarget(ctx, t.ID)
	if err != nil || a == nil {
		log.ErrorContext(ctx, "read attempt", "error", err)
		return
	}
	repo, originURL, err := i.repository(ctx, t.Repository)
	if err != nil {
		log.ErrorContext(ctx, "resolve repository", "error", err)
		return
	}
	ft, err := worker.ReadForgeToml(repo.Path)
	if err != nil {
		log.WarnContext(ctx, "forge.toml unreadable", "error", err)
	}
	if ft == nil || ft.IntegrationBranch == "" {
		i.refuse(ctx, t, "repository declares no integration_branch in forge.toml; nothing may be pushed (constitution 10)")
		return
	}
	if a.Branch == "" || a.HeadCommit == "" {
		i.refuse(ctx, t, "attempt recorded no task branch head; nothing to merge")
		return
	}

	// queued_for_merge → merging, and the merges row (rebase_attempts++).
	var m *store.Merge
	err = i.st.Write(ctx, func(tx *store.Tx) error {
		if _, terr := tx.Transition(ctx, t.ID, model.Merging, store.TransitionOptions{Actor: "integrator"}); terr != nil {
			return terr
		}
		m, err = tx.BeginMerge(ctx, t.ID, t.Repository, ft.IntegrationBranch)
		return err
	})
	if err != nil {
		log.ErrorContext(ctx, "begin merge", "error", err)
		return
	}
	log.InfoContext(ctx, "merging", "branch", a.Branch, "head", a.HeadCommit, "integration_branch", ft.IntegrationBranch, "rebase_attempts", m.RebaseAttempts)

	var out mergeOutcome
	if m.RebaseAttempts > i.cfg.MaxRebaseAttempts {
		out = mergeOutcome{outcome: outcomeConflict, reason: fmt.Sprintf("max_rebase_attempts %d exceeded", i.cfg.MaxRebaseAttempts)}
	} else {
		out = i.merge(ctx, log, t, a, repo.Path, originURL, ft)
	}
	i.landOutcome(ctx, log, t, a, m, out)
}

// landOutcome maps a mergeOutcome to the Target transition, the merges row,
// and the one-time integration-facts fill.
func (i *Integrator) landOutcome(ctx context.Context, log *slog.Logger, t store.Target, a *store.Attempt, m *store.Merge, out mergeOutcome) {
	detail := map[string]any{"target_id": t.ID, "reason": out.reason, "conflicted": out.conflicted, "scratch": out.scratch, "branch": m.IntegrationBranch, "repository": t.Repository}
	switch out.outcome {
	case outcomeMerged:
		err := i.st.Write(ctx, func(tx *store.Tx) error {
			if err := tx.FinishMerge(ctx, m.ID, outcomeMerged, out.before, out.after, detail); err != nil {
				return err
			}
			if _, err := tx.Transition(ctx, t.ID, model.Merged, store.TransitionOptions{Actor: "integrator"}); err != nil {
				return err
			}
			return i.fillFacts(ctx, tx, a, m, outcomeMerged, out.touched)
		})
		if err != nil {
			log.ErrorContext(ctx, "record merged outcome", "error", err)
			return
		}
		log.InfoContext(ctx, "merged", "before", out.before, "after", out.after, "branch", m.IntegrationBranch)
	case outcomeChecksFail:
		err := i.st.Write(ctx, func(tx *store.Tx) error {
			if err := tx.FinishMerge(ctx, m.ID, outcomeChecksFail, out.before, out.after, detail); err != nil {
				return err
			}
			if _, err := tx.Transition(ctx, t.ID, model.Unverified, store.TransitionOptions{UnverifiedReason: "check_failed:" + out.checkName, Actor: "integrator"}); err != nil {
				return err
			}
			return i.fillFacts(ctx, tx, a, m, outcomeChecksFail, out.touched)
		})
		if err != nil {
			log.ErrorContext(ctx, "record checks_failed outcome", "error", err)
			return
		}
		log.WarnContext(ctx, "checks failed on rebased result", "check", out.checkName)
	case outcomeConflict:
		err := i.st.Write(ctx, func(tx *store.Tx) error {
			if err := tx.FinishMerge(ctx, m.ID, outcomeConflict, "", "", detail); err != nil {
				return err
			}
			// Retained: the scratch clone (and the attempt's worktree) stay
			// for the human; `forge task requeue` re-enters the queue.
			_, err := tx.Transition(ctx, t.ID, model.Conflict, store.TransitionOptions{Actor: "integrator", Retained: out.scratch != ""})
			return err
		})
		if err != nil {
			log.ErrorContext(ctx, "record conflict outcome", "error", err)
			return
		}
		log.WarnContext(ctx, "conflict; human queue", "reason", out.reason, "conflicted", strings.Join(out.conflicted, ","), "scratch", out.scratch)
	default:
		// Infrastructure trouble (an unreachable remote, a failed clone):
		// back to the queue; BeginMerge's attempt counter caps the retries.
		err := i.st.Write(ctx, func(tx *store.Tx) error {
			if err := tx.Journal(ctx, "merge.retry", store.EntityTarget, t.ID, map[string]any{"reason": out.reason}); err != nil {
				return err
			}
			_, terr := tx.Transition(ctx, t.ID, model.QueuedForMerge, store.TransitionOptions{Actor: "integrator"})
			return terr
		})
		if err != nil {
			log.ErrorContext(ctx, "requeue after retryable failure", "error", err)
			return
		}
		log.WarnContext(ctx, "merge deferred", "reason", out.reason)
	}
}

// fillFacts is the one-time NULL → value fill (DESIGN.md §9.2). A missing
// facts row (possible when the queue was driven outside the daemon's
// completion path) is logged, never invented.
func (i *Integrator) fillFacts(ctx context.Context, tx *store.Tx, a *store.Attempt, m *store.Merge, outcome string, touched []string) error {
	f := store.IntegrationFacts{RebaseAttempts: &m.RebaseAttempts, MergeOutcome: outcome, TouchedPaths: touched}
	depth := 0
	if a.StackBaseCommit != "" {
		depth = 1
	}
	f.StackDepth = &depth
	if !a.FinishedAt.IsZero() {
		// Wall clock: the attempt finished in another process (§9.2).
		wait := i.clock().UTC().Sub(a.FinishedAt).Microseconds()
		if wait >= 0 {
			f.MergeWaitUS = &wait
		}
	}
	err := tx.UpdateIntegrationFacts(ctx, a.ID, f)
	if errors.Is(err, store.ErrNotFound) || errors.Is(err, store.ErrConflict) {
		i.log.WarnContext(ctx, "integration facts not filled", "attempt_id", a.ID, "error", err)
		return nil
	}
	return err
}

// refuse lands a Target in conflict without any git work, journaling why —
// the human queue is the honest place for "cannot merge by policy".
func (i *Integrator) refuse(ctx context.Context, t store.Target, reason string) {
	err := i.st.Write(ctx, func(tx *store.Tx) error {
		if err := tx.Journal(ctx, "merge.refused", store.EntityTarget, t.ID, map[string]string{"reason": reason}); err != nil {
			return err
		}
		if _, err := tx.Transition(ctx, t.ID, model.Merging, store.TransitionOptions{Actor: "integrator"}); err != nil {
			return err
		}
		_, err := tx.Transition(ctx, t.ID, model.Conflict, store.TransitionOptions{Actor: "integrator"})
		return err
	})
	if err != nil {
		i.log.ErrorContext(ctx, "refuse merge", "target_id", t.ID, "error", err)
		return
	}
	i.log.WarnContext(ctx, "merge refused", "target_id", t.ID, "reason", reason)
}

// land is a plain transition with logging (the cancel path).
func (i *Integrator) land(ctx context.Context, t store.Target, to model.State, opts store.TransitionOptions) {
	err := i.st.Write(ctx, func(tx *store.Tx) error {
		_, terr := tx.Transition(ctx, t.ID, to, opts)
		return terr
	})
	if err != nil {
		i.log.ErrorContext(ctx, "transition", "target_id", t.ID, "to", to, "error", err)
		return
	}
	i.log.InfoContext(ctx, "target moved", "target_id", t.ID, "to", to)
}

// repository resolves a registered repository row and its real origin URL —
// read-only inspection of the checkout (constitution 1).
func (i *Integrator) repository(ctx context.Context, name string) (*store.Repository, string, error) {
	repos, err := i.st.Repositories(ctx)
	if err != nil {
		return nil, "", err
	}
	for idx := range repos {
		if repos[idx].Name != name {
			continue
		}
		out, err := i.git.Run(ctx, repos[idx].Path, "remote", "get-url", "origin")
		if err != nil {
			return nil, "", fmt.Errorf("origin of %s: %w", name, err)
		}
		return &repos[idx], strings.TrimSpace(out), nil
	}
	return nil, "", fmt.Errorf("repository %s is not registered", name)
}

// merge does the git work in a scratch clone: clone the checkout (reads
// only), fetch the real origin's integration branch, rebase the task head
// onto it with zdiff3 + mergiraf solve, run the declared checks, and push
// through the policy gate.
func (i *Integrator) merge(ctx context.Context, log *slog.Logger, t store.Target, a *store.Attempt, checkoutPath, originURL string, ft *worker.ForgeToml) mergeOutcome {
	ib := ft.IntegrationBranch
	scratch := filepath.Join(i.cfg.Home, scratchDirName, t.Repository, model.ShortID(t.ID))
	if err := os.RemoveAll(scratch); err != nil {
		return mergeOutcome{reason: "clear scratch: " + err.Error()}
	}
	if err := os.MkdirAll(filepath.Dir(scratch), 0o700); err != nil {
		return mergeOutcome{reason: "create scratch parent: " + err.Error()}
	}
	if _, err := i.git.Run(ctx, filepath.Dir(scratch), "clone", "--no-tags", checkoutPath, scratch); err != nil {
		return mergeOutcome{reason: "clone checkout: " + err.Error()}
	}
	if _, err := i.git.Run(ctx, scratch, "remote", "add", "real", originURL); err != nil {
		return mergeOutcome{reason: "add remote: " + err.Error()}
	}
	if _, err := i.git.Run(ctx, scratch, "fetch", "--no-tags", "real", ib+":refs/remotes/real/"+ib); err != nil {
		return mergeOutcome{reason: "fetch " + ib + ": " + err.Error()}
	}
	before, err := i.revParse(ctx, scratch, "refs/remotes/real/"+ib)
	if err != nil {
		return mergeOutcome{reason: "resolve integration head: " + err.Error()}
	}
	if _, err := i.git.Run(ctx, scratch, "rev-parse", "--verify", a.HeadCommit+"^{commit}"); err != nil {
		// The task branch lives in the checkout; the clone should carry it.
		// A missing head means the branch is gone — a human's territory.
		return mergeOutcome{outcome: outcomeConflict, reason: "task head " + a.HeadCommit + " not found in checkout"}
	}
	if _, err := i.git.Run(ctx, scratch, "checkout", "-q", "--detach", a.HeadCommit); err != nil {
		return mergeOutcome{reason: "checkout task head: " + err.Error()}
	}

	if out, done := i.rebase(ctx, log, scratch, "refs/remotes/real/"+ib); done {
		out.scratch = scratch
		return out
	}
	after, err := i.revParse(ctx, scratch, "HEAD")
	if err != nil {
		return mergeOutcome{reason: "resolve rebased head: " + err.Error()}
	}
	touched, err := i.touchedPaths(ctx, scratch, before, after)
	if err != nil {
		log.WarnContext(ctx, "touched paths after rebase", "error", err)
		touched = nil
	}

	// The declared checks, on the actual merge result (constitution 10).
	scratchFT, err := worker.ReadForgeToml(scratch)
	if err != nil {
		log.WarnContext(ctx, "forge.toml in scratch clone unreadable", "error", err)
		scratchFT = ft
	}
	if scratchFT == nil {
		scratchFT = ft
	}
	for _, res := range worker.RunChecks(ctx, scratch, scratchFT, os.Environ()) {
		if !res.Passed {
			return mergeOutcome{outcome: outcomeChecksFail, checkName: res.Check, reason: "check failed: " + res.Check, before: before, after: after, touched: touched}
		}
	}

	if err := PushIntegration(ctx, i.git, scratch, originURL, ib, ft); err != nil {
		if errors.Is(err, ErrPushRefused) {
			return mergeOutcome{outcome: outcomeConflict, reason: err.Error()}
		}
		return mergeOutcome{reason: err.Error()} // a refused non-ff push retries; the counter caps it
	}
	if err := os.RemoveAll(scratch); err != nil {
		log.WarnContext(ctx, "remove scratch clone", "path", scratch, "error", err)
	}
	return mergeOutcome{outcome: outcomeMerged, before: before, after: after, touched: touched}
}

// rebase runs the rebase loop: on each conflict, mergiraf solve resolves the
// conflicted files syntactically; anything it cannot solve aborts nothing —
// the conflicted state is the retained evidence. done=true means the caller
// has a final (non-merged) outcome.
func (i *Integrator) rebase(ctx context.Context, log *slog.Logger, dir, onto string) (mergeOutcome, bool) {
	_, err := i.git.Run(ctx, dir, "rebase", onto)
	for round := 0; err != nil; round++ {
		if round >= maxRebaseRounds {
			return mergeOutcome{outcome: outcomeConflict, reason: "rebase did not converge"}, true
		}
		conflicted, cerr := i.conflictedFiles(ctx, dir)
		if cerr != nil || len(conflicted) == 0 {
			return mergeOutcome{outcome: outcomeConflict, reason: "rebase failed: " + err.Error()}, true
		}
		var unresolved []string
		for _, f := range conflicted {
			if serr := i.mergirafSolve(ctx, dir, f); serr != nil {
				log.DebugContext(ctx, "mergiraf could not solve", "file", f, "error", serr)
				unresolved = append(unresolved, f)
				continue
			}
			if _, aerr := i.git.Run(ctx, dir, "add", "--", f); aerr != nil {
				return mergeOutcome{outcome: outcomeConflict, reason: "stage resolution: " + aerr.Error(), conflicted: conflicted}, true
			}
		}
		if len(unresolved) > 0 {
			return mergeOutcome{outcome: outcomeConflict, reason: "conflict beyond mergiraf", conflicted: unresolved}, true
		}
		log.InfoContext(ctx, "mergiraf resolved conflict", "files", strings.Join(conflicted, ","))
		_, err = i.git.Run(ctx, dir, "rebase", "--continue")
	}
	return mergeOutcome{}, false
}

// mergirafSolve resolves one conflicted file in place; a non-zero exit (or a
// missing binary) means "not solved".
func (i *Integrator) mergirafSolve(ctx context.Context, dir, file string) error {
	cctx, cancel := context.WithTimeout(ctx, time.Minute)
	defer cancel()
	cmd := exec.CommandContext(cctx, mergirafBinary, "solve", "--keep-backup=false", file)
	cmd.Dir = dir
	out, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("mergiraf solve %s: %w: %s", file, err, strings.TrimSpace(string(out)))
	}
	return nil
}

func (i *Integrator) conflictedFiles(ctx context.Context, dir string) ([]string, error) {
	out, err := i.git.Run(ctx, dir, "diff", "--name-only", "--diff-filter=U")
	if err != nil {
		return nil, err
	}
	var files []string
	for _, line := range strings.Split(out, "\n") {
		if line = strings.TrimSpace(line); line != "" {
			files = append(files, line)
		}
	}
	return files, nil
}

// touchedPaths lists what the rebased result changes relative to the old
// integration head — the honest touched_paths after a rebase (§9.2).
func (i *Integrator) touchedPaths(ctx context.Context, dir, before, after string) ([]string, error) {
	out, err := i.git.Run(ctx, dir, "diff", "--name-only", before+".."+after)
	if err != nil {
		return nil, err
	}
	var files []string
	for _, line := range strings.Split(out, "\n") {
		if line = strings.TrimSpace(line); line != "" {
			files = append(files, line)
		}
	}
	return files, nil
}

func (i *Integrator) revParse(ctx context.Context, dir, ref string) (string, error) {
	out, err := i.git.Run(ctx, dir, "rev-parse", "--verify", ref+"^{commit}")
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(out), nil
}
