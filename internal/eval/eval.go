// Package eval runs golden task fixtures (evals/<mode>/<case>/eval.toml)
// through a real daemon + worker pair whose only executor replays fake-claude
// fixtures, and scores the outcomes against each case's expectations
// (DESIGN.md §23). Isolation is by process: each case gets its own temporary
// FORGE_HOME and its own `forge daemon start --foreground` child (which
// spawns the worker), because the fake executor's fixture travels in the
// child's environment — one in-process pair could not pin a different
// fixture per case.
package eval

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/BurntSushi/toml"
)

// Expect is a case's [expect] table: what the finished task must look like.
type Expect struct {
	State         string `toml:"state"`          // succeeded | failed | unverified | cancelled | partial | merged
	MinClaims     int    `toml:"min_claims"`     // at least this many claims in the result envelope
	Verified      *bool  `toml:"verified"`       // the attempt's verification_passed, when set
	FailureReason string `toml:"failure_reason"` // the target's failure_reason, when set
}

// Case is one golden task: a prompt, the fixture that answers it, and the
// outcome it must reach.
type Case struct {
	Name        string `toml:"name"`
	Mode        string `toml:"mode"`
	Prompt      string `toml:"prompt"`
	Fixture     string `toml:"fixture"`      // a fake-claude fixture name under the fixtures dir
	RepoFixture string `toml:"repo_fixture"` // "git-basic": a generated tiny git repository
	Autonomy    string `toml:"autonomy"`     // optional; "" takes the ad-hoc default
	Expect      Expect `toml:"expect"`

	// Dir is where eval.toml was read from; not part of the file.
	Dir string `toml:"-"`
}

// terminalStates are the Work states a case may expect and a run may end in.
var terminalStates = map[string]bool{
	"succeeded": true, "failed": true, "unverified": true,
	"cancelled": true, "partial": true, "merged": true,
}

func (c *Case) validate() error {
	for name, v := range map[string]string{"name": c.Name, "mode": c.Mode, "prompt": c.Prompt, "fixture": c.Fixture} {
		if v == "" {
			return fmt.Errorf("%s is required", name)
		}
	}
	if c.RepoFixture != "" && c.RepoFixture != "git-basic" {
		return fmt.Errorf("repo_fixture %q: only git-basic is generated", c.RepoFixture)
	}
	if !terminalStates[c.Expect.State] {
		return fmt.Errorf("expect.state %q: want a terminal work state", c.Expect.State)
	}
	if c.Expect.MinClaims < 0 {
		return fmt.Errorf("expect.min_claims %d is negative", c.Expect.MinClaims)
	}
	return nil
}

// LoadCases reads every <casesDir>/<mode>/<case>/eval.toml, sorted by case
// name. Unknown keys are refused so a typo fails loudly.
func LoadCases(casesDir, mode string) ([]Case, error) {
	dir := filepath.Join(casesDir, mode)
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, fmt.Errorf("read cases for mode %s: %w", mode, err)
	}
	var cases []Case
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		path := filepath.Join(dir, e.Name(), "eval.toml")
		var c Case
		md, err := toml.DecodeFile(path, &c)
		if err != nil {
			return nil, fmt.Errorf("case %s: %w", e.Name(), err)
		}
		if undecoded := md.Undecoded(); len(undecoded) > 0 {
			return nil, fmt.Errorf("case %s: unknown key %s", e.Name(), undecoded[0])
		}
		c.Dir = filepath.Dir(path)
		if err := c.validate(); err != nil {
			return nil, fmt.Errorf("case %s: %w", e.Name(), err)
		}
		if c.Mode != mode {
			return nil, fmt.Errorf("case %s: mode %q does not match its directory %s", c.Name, c.Mode, mode)
		}
		cases = append(cases, c)
	}
	if len(cases) == 0 {
		return nil, fmt.Errorf("no cases under %s", dir)
	}
	sort.Slice(cases, func(i, j int) bool { return cases[i].Name < cases[j].Name })
	return cases, nil
}

// Options are Run's inputs. ForgeBin, CasesDir, FixturesDir, and WorkDir are
// required; the rest default.
type Options struct {
	Mode              string
	Model             string // model alias submitted with each task; default haiku
	PromptVersionHash string // recorded in the report only; the cases carry their own prompts
	CasesDir          string
	FixturesDir       string        // where fake-claude fixture directories live
	ForgeBin          string        // the forge binary each case's daemon and worker run
	WorkDir           string        // per-case temporary homes are created under it
	Timeout           time.Duration // per case; default 120s
	Logger            *slog.Logger
}

// Result is one case's outcome.
type Result struct {
	Name          string  `json:"name"`
	Pass          bool    `json:"pass"`
	State         string  `json:"state"`
	FailureReason string  `json:"failure_reason,omitempty"`
	Turns         int     `json:"turns"`
	CostUSD       float64 `json:"cost_usd"`
	Details       string  `json:"details,omitempty"`
}

// Report is the whole run: per-case results and the summary score (the pass
// fraction), which is what `forge eval --record-proposal` posts.
type Report struct {
	Mode              string   `json:"mode"`
	Model             string   `json:"model"`
	PromptVersionHash string   `json:"prompt_version_hash,omitempty"`
	Cases             []Result `json:"cases"`
	Score             float64  `json:"score"`
}

// Run executes every case for the mode, sequentially (each case owns a
// daemon, a worker, and the fixture environment variable), and scores them.
// A case that cannot even run counts as a failure with the error in Details;
// only a problem with the case set itself is an error.
func Run(ctx context.Context, o Options) (*Report, error) {
	if o.Model == "" {
		o.Model = "haiku"
	}
	if o.Timeout <= 0 {
		o.Timeout = 120 * time.Second
	}
	if o.Logger == nil {
		o.Logger = slog.New(slog.DiscardHandler)
	}
	for name, v := range map[string]string{"mode": o.Mode, "cases dir": o.CasesDir, "fixtures dir": o.FixturesDir, "forge binary": o.ForgeBin, "work dir": o.WorkDir} {
		if v == "" {
			return nil, fmt.Errorf("eval: %s is required", name)
		}
	}
	cases, err := LoadCases(o.CasesDir, o.Mode)
	if err != nil {
		return nil, err
	}
	rep := &Report{Mode: o.Mode, Model: o.Model, PromptVersionHash: o.PromptVersionHash}
	passes := 0
	for _, c := range cases {
		start := time.Now()
		res := runCase(ctx, o, c)
		o.Logger.InfoContext(ctx, "eval case finished", "case", c.Name, "pass", res.Pass, "state", res.State, "duration_us", time.Since(start).Microseconds())
		if res.Pass {
			passes++
		}
		rep.Cases = append(rep.Cases, res)
	}
	rep.Score = float64(passes) / float64(len(rep.Cases))
	return rep, nil
}

// score compares one finished task view against the case's expectations and
// returns pass plus a human line naming every mismatch.
func score(c Case, v *taskView) (bool, string) {
	var problems []string
	if string(v.State) != c.Expect.State {
		problems = append(problems, fmt.Sprintf("state %s, want %s", v.State, c.Expect.State))
	}
	if c.Expect.FailureReason != "" {
		got := ""
		if len(v.Targets) > 0 {
			got = v.Targets[0].FailureReason
		}
		if got != c.Expect.FailureReason {
			problems = append(problems, fmt.Sprintf("failure_reason %q, want %q", got, c.Expect.FailureReason))
		}
	}
	last := v.lastAttempt()
	if c.Expect.MinClaims > 0 {
		n := 0
		if last != nil {
			n = claimCount(last.Result)
		}
		if n < c.Expect.MinClaims {
			problems = append(problems, fmt.Sprintf("%d claim(s), want at least %d", n, c.Expect.MinClaims))
		}
	}
	if c.Expect.Verified != nil {
		got := last != nil && last.VerificationPassed != nil && *last.VerificationPassed
		if got != *c.Expect.Verified {
			problems = append(problems, fmt.Sprintf("verified %v, want %v", got, *c.Expect.Verified))
		}
	}
	return len(problems) == 0, strings.Join(problems, "; ")
}

// claimCount reads the claims array of a result envelope; anything unparseable
// counts as zero claims.
func claimCount(envelope json.RawMessage) int {
	var v struct {
		Claims []json.RawMessage `json:"claims"`
	}
	if err := json.Unmarshal(envelope, &v); err != nil {
		return 0
	}
	return len(v.Claims)
}
