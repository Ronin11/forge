package worker

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"sort"
	"strings"
	"syscall"
	"time"

	"github.com/BurntSushi/toml"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
)

// ForgeToml is the optional file inside a repository (DESIGN.md §3).
// IntegrationBranch and TaskBranches are the M9 push policy (constitution 10):
// Forge pushes only to a branch this file lists.
type ForgeToml struct {
	Checks map[string][]string `toml:"checks"`
	// Setup is run once before the checks in a tree that has never built —
	// dependency install, codegen. The integrator's scratch clone needs it
	// (a worker's worktree usually has the agent's own install); a check
	// like "npm run build" is meaningless in a clone with no node_modules.
	Setup    []string            `toml:"setup"`
	Defaults struct {
		Autonomy   string `toml:"autonomy"`
		BaseBranch string `toml:"base_branch"`
	} `toml:"defaults"`
	IntegrationBranch string `toml:"integration_branch"`
	TaskBranches      string `toml:"task_branches"`
	Modes             map[string]struct {
		Paths []string `toml:"paths"`
	} `toml:"modes"`
	Verify struct {
		UI bool `toml:"ui"`
	} `toml:"verify"`
	CheckTimeouts map[string]int `toml:"check_timeouts"`
	// Run is the app lifecycle Forge can drive with no agent in the loop: the
	// Repos page's Start/Stop/Rebuild just run these commands. The daemon leases
	// a free port, injects it as PortEnv, and treats the app as up once ReadyLog
	// appears (or ReadyTimeout elapses). See docs and RepoRun.
	Run RepoRun `toml:"run"`
}

// RepoRun is the [run] table of .forge/config.toml.
type RepoRun struct {
	Build         []string          `toml:"build"` // one-off build (Rebuild runs this)
	Start         []string          `toml:"start"` // the long-running command (a dev server)
	Stop          []string          `toml:"stop"`  // optional graceful stop; else the group is signalled
	HotReload     bool              `toml:"hot_reload"`
	PortEnv       string            `toml:"port_env"`      // env var the leased port is injected as, e.g. "PORT"
	HealthPath    string            `toml:"health_path"`   // path under the app URL for a readiness check
	ReadyLog      string            `toml:"ready_log"`     // stdout/stderr substring that signals "up"
	ReadyTimeoutS int               `toml:"ready_timeout"` // seconds to wait for ready before giving up (default 60)
	Env           map[string]string `toml:"env"`           // extra environment for the app
}

// ReadForgeToml reads the repository's Forge configuration: .forge/config.toml
// wins over a top-level forge.toml (the per-repo .forge/ directory is the
// repo-scoped home — config here, mode prompt overlays in .forge/modes/, notes
// in .forge/notes/); absent is (nil, nil).
func ReadForgeToml(worktree string) (*ForgeToml, error) {
	for _, rel := range []string{filepath.Join(".forge", "config.toml"), "forge.toml"} {
		path := filepath.Join(worktree, rel)
		var ft ForgeToml
		if _, err := toml.DecodeFile(path, &ft); err != nil {
			if os.IsNotExist(err) {
				continue
			}
			return nil, fmt.Errorf("read %s: %w", path, err)
		}
		return &ft, nil
	}
	return nil, nil
}

// CheckResult is one declared check as Forge ran it (VERIFICATION.md L1).
type CheckResult struct {
	Check      string   `json:"check"`
	Passed     bool     `json:"passed"`
	DurationUS int64    `json:"duration_us"`
	ExitCode   int      `json:"exit_code"`
	OutputTail string   `json:"output_tail"`
	Failing    []string `json:"failing_tests,omitempty"`
}

// checkTailBytes is how much of a check's output Forge keeps: the tail, which
// is where a test runner puts its failures.
const checkTailBytes = 64 << 10

// RunChecks runs every declared check in the worktree, in name order, each in
// its own process group with a timeout, and returns all results — Forge's own
// measurement, never the agent's claim.
func RunChecks(ctx context.Context, worktree string, ft *ForgeToml, env []string) []CheckResult {
	if ft == nil || len(ft.Checks) == 0 {
		return nil
	}
	names := make([]string, 0, len(ft.Checks))
	for n := range ft.Checks {
		names = append(names, n)
	}
	sort.Strings(names)
	var out []CheckResult
	for _, name := range names {
		out = append(out, runCheck(ctx, worktree, name, ft.Checks[name], ft.CheckTimeouts[name], env))
	}
	return out
}

func runCheck(ctx context.Context, worktree, name string, argv []string, timeoutSec int, env []string) CheckResult {
	res := CheckResult{Check: name}
	if len(argv) == 0 {
		res.OutputTail = "empty command"
		return res
	}
	timeout := 10 * time.Minute
	if timeoutSec > 0 {
		timeout = time.Duration(timeoutSec) * time.Second
	}
	cctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	cmd := exec.CommandContext(cctx, argv[0], argv[1:]...)
	cmd.Dir = worktree
	cmd.Env = env
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	cmd.Cancel = func() error { return syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL) }
	start := time.Now()
	tail, err := runCheckProcess(cmd)
	res.DurationUS = time.Since(start).Microseconds()
	res.OutputTail = tail
	if err == nil {
		res.Passed = true
		return res
	}
	var exit *exec.ExitError
	if errors.As(err, &exit) {
		res.ExitCode = exit.ExitCode()
	} else {
		res.ExitCode = -1
		res.OutputTail = err.Error() + "\n" + res.OutputTail
	}
	res.Failing = failingTests(tail)
	return res
}

// runCheckProcess runs one check and returns the tail of its output and the
// command's own error. Output goes to a pipe this function owns rather than to
// CombinedOutput's buffer, because of what a build does every day: a check that
// backgrounds a server (`npm run preview &`) hands that server the same
// stdout, and CombinedOutput would then wait for the server rather than for
// the check. Once the check has exited, its process group is killed, so the
// server does not outlive it either; a failure to do so is reported in the
// output tail, where an operator reading the check will see it, and never
// turns a passing check into a failing one.
func runCheckProcess(cmd *exec.Cmd) (string, error) {
	pr, pw, err := os.Pipe()
	if err != nil {
		return "", fmt.Errorf("output pipe: %w", err)
	}
	cmd.Stdout, cmd.Stderr = pw, pw
	if err := cmd.Start(); err != nil {
		return "", errors.Join(fmt.Errorf("start %s: %w", cmd.Path, err), pw.Close(), pr.Close())
	}
	pid := cmd.Process.Pid
	pidStart, startErr := ProcessStart(pid)
	// The child owns the write end now; the parent's copy would hide the EOF.
	notes := pw.Close()
	tail := &tailWriter{max: checkTailBytes}
	drained := make(chan struct{})
	go func() {
		defer close(drained)
		if _, err := io.Copy(tail, pr); err != nil && !errors.Is(err, fs.ErrClosed) {
			tail.note(fmt.Errorf("read check output: %w", err))
		}
	}()
	waitErr := cmd.Wait()
	if startErr != nil {
		notes = errors.Join(notes, startErr)
	} else if err := KillGroup(pid, pidStart, killGrace); err != nil {
		notes = errors.Join(notes, err)
	}
	// The group is gone, so the pipe reaches EOF; the grace is for the case
	// where the kill could not happen (an identity that changed under us).
	timer := time.NewTimer(drainGrace)
	defer timer.Stop()
	select {
	case <-drained:
	case <-timer.C:
		notes = errors.Join(notes, closeQuiet(pr))
		<-drained
	}
	notes = errors.Join(notes, closeQuiet(pr))
	if notes != nil {
		tail.note(notes)
	}
	return tail.string(), waitErr
}

// tailWriter keeps the last max bytes written to it: the tail is all Forge
// records of a check, so nothing longer is held in memory.
type tailWriter struct {
	max int
	buf []byte
}

func (w *tailWriter) Write(p []byte) (int, error) {
	n := len(p)
	if len(p) > w.max {
		p = p[len(p)-w.max:]
	}
	w.buf = append(w.buf, p...)
	if excess := len(w.buf) - w.max; excess > 0 {
		w.buf = append(w.buf[:0], w.buf[excess:]...)
	}
	return n, nil
}

// note appends one Forge-attributed line to the output, for what happened
// around the check rather than inside it.
func (w *tailWriter) note(err error) {
	if _, werr := w.Write([]byte("\n[forge] " + err.Error() + "\n")); werr != nil {
		panic("worker: tailWriter.Write returned an error")
	}
}

func (w *tailWriter) string() string { return string(w.buf) }

// failingTests extracts test names from the formats Forge recognises; unknown
// output yields nothing rather than a guess.
func failingTests(out string) []string {
	var names []string
	for _, line := range strings.Split(out, "\n") {
		line = strings.TrimSpace(line)
		switch {
		case strings.HasPrefix(line, "--- FAIL: "): // go test
			names = append(names, strings.Fields(strings.TrimPrefix(line, "--- FAIL: "))[0])
		case strings.HasPrefix(line, "FAILED ") && strings.Contains(line, "::"): // pytest
			names = append(names, strings.Fields(strings.TrimPrefix(line, "FAILED "))[0])
		case strings.HasPrefix(line, "✕ ") || strings.HasPrefix(line, "✗ "): // jest
			names = append(names, strings.TrimSpace(line[len("✕ "):]))
		}
	}
	return names
}

// The envelope types live in protocol so the daemon, modes, and worker share
// one definition; these aliases keep this package's vocabulary.
type (
	ResultEnvelope = protocol.ResultEnvelope
	NeedsInput     = protocol.NeedsInput
	ResultChange   = protocol.ResultChange
	ResultCheckRun = protocol.ResultCheckRun
	ResultClaim    = protocol.ResultClaim
)

// EnvelopeSchema is the common result contract, passed as --json-schema for
// attempts whose autonomy allows questions so a needs_input pause is enforced
// by the CLI rather than hoped for from the prompt (MODES.md). The per-mode
// schemas of M4 extend this envelope.
const EnvelopeSchema = `{"type":"object","additionalProperties":false,"required":["schema_version","summary","needs_input","changes","checks_run","claims"],"properties":{` +
	`"schema_version":{"type":"integer"},` +
	`"summary":{"type":"string"},` +
	`"needs_input":{"anyOf":[{"type":"null"},{"type":"object","additionalProperties":false,"required":["question"],"properties":{"question":{"type":"string"},"options":{"type":"array","items":{"type":"string"}},"context":{"type":"string"},"checkpoint":{"type":["string","null"]}}}]},` +
	`"changes":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["path","kind"],"properties":{"path":{"type":"string"},"kind":{"type":"string","enum":["added","modified","deleted"]},"summary":{"type":"string"}}}},` +
	`"checks_run":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["check","passed"],"properties":{"check":{"type":"string"},"passed":{"type":"boolean"},"notes":{"type":"string"}}}},` +
	`"claims":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["claim","evidence"],"properties":{"claim":{"type":"string"},"evidence":{"type":"string"}}}}}}`

// ParseEnvelope decodes a structured result; ok=false when there is none.
func ParseEnvelope(raw json.RawMessage) (*ResultEnvelope, bool, error) {
	if len(raw) == 0 {
		return nil, false, nil
	}
	var env ResultEnvelope
	if err := json.Unmarshal(raw, &env); err != nil {
		return nil, true, fmt.Errorf("structured result: %w", err)
	}
	return &env, true, nil
}

// DefaultDocsGlobs is what docs_only enforces when the repository declares no
// [modes.docs] paths (MODES.md §docs).
var DefaultDocsGlobs = []string{"docs/**", "*.md"}

// Verify applies L0 and L1 (VERIFICATION.md) from what the worker measured:
// changes[]-vs-git consistency, the mode's write scope against git, claim
// evidence, then the declared checks against the agent's checks_run claims. An
// L0 failure stops at level 0; L2/L3 are the daemon's to orchestrate. The
// first failure sets the reason; the verdict records every check either way.
func Verify(env *ResultEnvelope, hasEnvelope bool, git protocol.GitOutcome, checks []CheckResult, declared bool, scope model.WriteScope, docsGlobs []string) protocol.Verification {
	v := protocol.Verification{Level: 0, Passed: true}
	fail := func(reason string) {
		if v.Passed {
			v.Passed, v.Reason = false, reason
		}
	}
	verdict := map[string]any{"l0_envelope": hasEnvelope, "l0_scope": string(scope)}

	// L0.2: changes[] ⊆ changed paths in git and changed paths ⊆ changes[].
	// A new_project build writes hundreds of files at once and reports them as a
	// coarse summary, so exactness there measures the summary, not the work: the
	// diffs are still recorded but never fail (VERIFICATION.md §L0). Greenfield
	// is an L2 mode, verified against its declared checks.
	if hasEnvelope && env != nil {
		// Analysis modes (supervise, verify follow-ups) legitimately dirty the
		// throwaway worktree by RUNNING the product under assessment — npm
		// installs, build artifacts. Their contract is "nothing ships",
		// enforced below by commits and declared changes, not by dirt: two
		// bench supervise rounds died l0:changes_mismatch on node_modules and
		// their revise verdicts were silently dropped (2026-09-05).
		exact := scope != model.WritesNewProject && scope != model.WritesNone && scope != model.WritesKbOnly
		verdict["l0_changes_exact"] = exact
		changed := map[string]bool{}
		for _, p := range git.ChangedPaths {
			changed[p] = true
		}
		var missing, undeclared []string
		claimed := map[string]bool{}
		for _, c := range env.Changes {
			claimed[c.Path] = true
			if !changed[c.Path] {
				missing = append(missing, c.Path)
			}
		}
		for p := range changed {
			if !claimed[p] {
				undeclared = append(undeclared, p)
			}
		}
		sort.Strings(missing)
		sort.Strings(undeclared)
		verdict["l0_claimed_not_changed"], verdict["l0_changed_not_claimed"] = missing, undeclared
		if exact && (len(missing) > 0 || len(undeclared) > 0) {
			// Imperfect changes[] bookkeeping on real work is a WARNING, not a
			// verdict: the exactness gate was discarding committed work
			// (13 attempts in one week; four with commits later reported as
			// missing deliverables — proposal 9a87814a). The verdict records
			// the discrepancy for audit either way; only a fabricated
			// envelope — changes claimed while git saw nothing at all —
			// still fails.
			verdict["l0_changes_warned"] = true
			if len(env.Changes) > 0 && !git.Dirty && git.Commits == 0 && len(git.ChangedPaths) == 0 {
				fail("l0:changes_fabricated")
			}
		}
	}

	// L0.3: write scope against what git measured.
	switch scope {
	case model.WritesNone, model.WritesKbOnly:
		// Uncommitted dirt is tolerated (see L0.2): running the assessed
		// product dirties the worktree and the worktree is discarded. Commits
		// and declared changes are the ship-shaped violations.
		var offending []string
		if git.Commits > 0 {
			offending = append(offending, fmt.Sprintf("%d commits", git.Commits))
		}
		if env != nil && len(env.Changes) > 0 {
			offending = append(offending, "changes[] not empty")
		}
		if len(offending) > 0 {
			offending = append(offending, git.ChangedPaths...)
			verdict["l0_scope_offending"] = offending
			fail("l0:scope:" + string(scope))
		}
	case model.WritesDocsOnly:
		globs := docsGlobs
		if len(globs) == 0 {
			globs = DefaultDocsGlobs
		}
		verdict["l0_docs_globs"] = globs
		var offending []string
		for _, p := range git.ChangedPaths {
			matched := false
			for _, g := range globs {
				if globMatch(g, p) {
					matched = true
					break
				}
			}
			if !matched {
				offending = append(offending, p)
			}
		}
		if len(offending) > 0 {
			sort.Strings(offending)
			verdict["l0_scope_offending"] = offending
			fail("l0:scope:" + string(scope))
		}
	default: // repo, new_project: no path restriction.
	}

	// L0.5: every claim carries evidence.
	if env != nil {
		var missing []string
		for _, cl := range env.Claims {
			if strings.TrimSpace(cl.Evidence) == "" {
				missing = append(missing, cl.Claim)
			}
		}
		if len(missing) > 0 {
			verdict["l0_claims_without_evidence"] = missing
			fail("l0:claims_evidence")
		}
	}
	if !v.Passed {
		// Stopped at L0: the level reports where verification got to.
		v.Verdict = marshalAttrs(verdict)
		return v
	}

	v.Level = 1
	if !declared {
		verdict["l1_vacuous"] = true
	} else {
		verdict["l1_checks"] = checks
		if env != nil {
			byName := map[string]bool{}
			for _, c := range checks {
				byName[c.Check] = c.Passed
			}
			// One-directional, and first: a claimed pass Forge cannot reproduce
			// is the canonical false claim (VERIFICATION.md L1 rule 2); an
			// unclaimed check is fine.
			for _, claim := range env.ChecksRun {
				if passed, ok := byName[claim.Check]; claim.Passed && ok && !passed {
					fail("check_claim_mismatch:" + claim.Check)
				}
			}
		}
		for _, c := range checks {
			if !c.Passed {
				fail("check_failed:" + c.Check)
			}
		}
	}
	v.Verdict = marshalAttrs(verdict)
	return v
}

// globMatch matches one changed path against a docs glob. path.Match's `*`
// never crosses a slash, so "<dir>/**" is implemented as the directory prefix:
// it matches anything under <dir>.
func globMatch(glob, p string) bool {
	if prefix, ok := strings.CutSuffix(glob, "/**"); ok {
		return p == prefix || strings.HasPrefix(p, prefix+"/")
	}
	ok, err := path.Match(glob, p)
	return err == nil && ok
}
