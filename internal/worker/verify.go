package worker

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"syscall"
	"time"

	"github.com/BurntSushi/toml"

	"forge/internal/protocol"
)

// ForgeToml is the optional file inside a repository (DESIGN.md §3).
type ForgeToml struct {
	Checks   map[string][]string `toml:"checks"`
	Defaults struct {
		Autonomy   string `toml:"autonomy"`
		BaseBranch string `toml:"base_branch"`
	} `toml:"defaults"`
	Modes map[string]struct {
		Paths []string `toml:"paths"`
	} `toml:"modes"`
	Verify struct {
		UI bool `toml:"ui"`
	} `toml:"verify"`
	CheckTimeouts map[string]int `toml:"check_timeouts"`
}

// ReadForgeToml reads <worktree>/forge.toml; absent is (nil, nil).
func ReadForgeToml(worktree string) (*ForgeToml, error) {
	path := filepath.Join(worktree, "forge.toml")
	var ft ForgeToml
	if _, err := toml.DecodeFile(path, &ft); err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("read %s: %w", path, err)
	}
	return &ft, nil
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
	outb, err := cmd.CombinedOutput()
	res.DurationUS = time.Since(start).Microseconds()
	tail := string(outb)
	if len(tail) > 64<<10 {
		tail = tail[len(tail)-64<<10:]
	}
	res.OutputTail = tail
	if err == nil {
		res.Passed = true
		return res
	}
	if exit, ok := err.(*exec.ExitError); ok {
		res.ExitCode = exit.ExitCode()
	} else {
		res.ExitCode = -1
		res.OutputTail = err.Error() + "\n" + res.OutputTail
	}
	res.Failing = failingTests(tail)
	return res
}

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

// ResultEnvelope is the common part of every mode's structured result.
type ResultEnvelope struct {
	SchemaVersion int              `json:"schema_version"`
	Summary       string           `json:"summary"`
	NeedsInput    *NeedsInput      `json:"needs_input"`
	Changes       []ResultChange   `json:"changes"`
	ChecksRun     []ResultCheckRun `json:"checks_run"`
	Claims        []ResultClaim    `json:"claims"`
}

// NeedsInput is the agent asking for a human.
type NeedsInput struct {
	Question   string          `json:"question"`
	Options    []string        `json:"options"`
	Context    json.RawMessage `json:"context"`
	Checkpoint string          `json:"checkpoint"`
}

// ResultChange, ResultCheckRun, ResultClaim are envelope items.
type ResultChange struct {
	Path    string `json:"path"`
	Kind    string `json:"kind"`
	Summary string `json:"summary"`
}
type ResultCheckRun struct {
	Check  string `json:"check"`
	Passed bool   `json:"passed"`
	Notes  string `json:"notes"`
}
type ResultClaim struct {
	Claim    string `json:"claim"`
	Evidence string `json:"evidence"`
}

// EnvelopeSchema is the common result contract, passed as --json-schema for
// attempts whose autonomy allows questions so a needs_input pause is enforced
// by the CLI rather than hoped for from the prompt (MODES.md). The per-mode
// schemas of M4 extend this envelope; until then every field an agent must
// fill is here.
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

// Verify applies L0 and L1 (VERIFICATION.md) from what the worker measured.
// M1 scope: L0 checks the envelope parses and its changes[] agree with git when
// present; L1 runs declared checks and compares them with checks_run claims.
// Modes with write scopes and L2 arrive in M4 through the same function.
func Verify(env *ResultEnvelope, hasEnvelope bool, git protocol.GitOutcome, checks []CheckResult, declared bool) protocol.Verification {
	v := protocol.Verification{Level: 0, Passed: true}
	verdict := map[string]any{}
	if hasEnvelope && env != nil {
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
		if len(missing) > 0 || len(undeclared) > 0 {
			v.Passed, v.Reason = false, "l0:changes_mismatch"
		}
	}
	verdict["l0_envelope"] = hasEnvelope
	if !declared {
		v.Level = 1
		verdict["l1_vacuous"] = true
	} else {
		v.Level = 1
		verdict["l1_checks"] = checks
		for _, c := range checks {
			if !c.Passed && v.Passed {
				v.Passed, v.Reason = false, "check_failed:"+c.Check
			}
		}
		if env != nil {
			byName := map[string]bool{}
			for _, c := range checks {
				byName[c.Check] = c.Passed
			}
			for _, claim := range env.ChecksRun {
				if passed, ok := byName[claim.Check]; claim.Passed && ok && !passed && v.Passed {
					v.Passed, v.Reason = false, "check_claim_mismatch:"+claim.Check
				}
			}
		}
	}
	v.Verdict = marshalAttrs(verdict)
	return v
}
