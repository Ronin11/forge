package eval

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"time"
)

// taskView is the slice of GET /api/v1/tasks/{id} eval reads, decoded from
// `forge task show --json` so eval depends on the CLI contract, not on
// internal packages.
type taskView struct {
	Work struct {
		ID string `json:"id"`
	} `json:"work"`
	State   string `json:"state"`
	Targets []struct {
		State         string `json:"state"`
		FailureReason string `json:"failure_reason"`
	} `json:"targets"`
	Attempts []struct {
		NumTurns           int             `json:"num_turns"`
		CostUSD            *float64        `json:"cost_usd"`
		Result             json.RawMessage `json:"result"`
		VerificationPassed *bool           `json:"verification_passed"`
	} `json:"attempts"`
}

func (v *taskView) lastAttempt() *struct {
	NumTurns           int             `json:"num_turns"`
	CostUSD            *float64        `json:"cost_usd"`
	Result             json.RawMessage `json:"result"`
	VerificationPassed *bool           `json:"verification_passed"`
} {
	if len(v.Attempts) == 0 {
		return nil
	}
	return &v.Attempts[len(v.Attempts)-1]
}

// runCase drives one case end to end: an isolated home, a daemon child (which
// spawns the worker), one submitted task, a terminal state, a score. Every
// failure lands in Details — one broken case never aborts the run.
func runCase(ctx context.Context, o Options, c Case) Result {
	res := Result{Name: c.Name, State: "error"}
	ctx, cancel := context.WithTimeout(ctx, o.Timeout)
	defer cancel()
	fixture, err := filepath.Abs(filepath.Join(o.FixturesDir, c.Fixture))
	if err == nil {
		if _, serr := os.Stat(filepath.Join(fixture, "meta.toml")); serr != nil {
			err = fmt.Errorf("fixture %s: %w", c.Fixture, serr)
		}
	}
	if err != nil {
		res.Details = err.Error()
		return res
	}
	home := filepath.Join(o.WorkDir, c.Name)
	repoName := "git-basic"
	repoPath, err := makeGitBasic(ctx, home)
	if err != nil {
		res.Details = err.Error()
		return res
	}
	if err := writeCaseConfig(home, o.ForgeBin, repoName, repoPath); err != nil {
		res.Details = err.Error()
		return res
	}
	env := caseEnv(home, fixture)
	stop, err := startDaemon(ctx, o.ForgeBin, home, env)
	if err != nil {
		res.Details = err.Error()
		return res
	}
	defer stop()
	view, err := submitAndWait(ctx, o, c, home, repoName, env)
	if err != nil {
		res.Details = err.Error()
		return res
	}
	res.State = view.State
	if len(view.Targets) > 0 {
		res.FailureReason = view.Targets[0].FailureReason
	}
	if last := view.lastAttempt(); last != nil {
		res.Turns = last.NumTurns
		if last.CostUSD != nil {
			res.CostUSD = *last.CostUSD
		}
	}
	res.Pass, res.Details = score(c, view)
	return res
}

// makeGitBasic generates the "git-basic" repository fixture: a bare origin
// and a clone with one pushed commit, all inside the case home. Real git,
// never a mock (STYLE.md §5).
func makeGitBasic(ctx context.Context, home string) (string, error) {
	origin := filepath.Join(home, "origin.git")
	repo := filepath.Join(home, "repos", "git-basic")
	if err := os.MkdirAll(filepath.Dir(repo), 0o700); err != nil {
		return "", fmt.Errorf("create repos dir: %w", err)
	}
	env := append(os.Environ(),
		"GIT_AUTHOR_NAME=forge-eval", "GIT_AUTHOR_EMAIL=eval@localhost",
		"GIT_COMMITTER_NAME=forge-eval", "GIT_COMMITTER_EMAIL=eval@localhost",
		"GIT_CONFIG_GLOBAL=/dev/null", "GIT_CONFIG_SYSTEM=/dev/null")
	git := func(dir string, args ...string) error {
		cmd := exec.CommandContext(ctx, "git", args...)
		cmd.Dir = dir
		cmd.Env = env
		if out, err := cmd.CombinedOutput(); err != nil {
			return fmt.Errorf("git %s: %w: %s", strings.Join(args, " "), err, bytes.TrimSpace(out))
		}
		return nil
	}
	if err := git(home, "init", "--bare", "-b", "main", origin); err != nil {
		return "", err
	}
	if err := git(home, "clone", origin, repo); err != nil {
		return "", err
	}
	if err := os.WriteFile(filepath.Join(repo, "README.md"), []byte("# git-basic\n\nA tiny generated repository for eval cases.\n"), 0o644); err != nil {
		return "", fmt.Errorf("write README: %w", err)
	}
	for _, args := range [][]string{
		{"add", "-A"},
		{"-c", "commit.gpgsign=false", "commit", "-q", "-m", "chore: seed git-basic"},
		{"push", "-q", "origin", "main"},
	} {
		if err := git(repo, args...); err != nil {
			return "", err
		}
	}
	return repo, nil
}

// writeCaseConfig writes the case's config.toml and worker.toml before the
// daemon starts (bootstrap never overwrites existing files). The one executor
// is named claude-code — the name ad-hoc submissions freeze — but its command
// is `forge fake-claude`, so the case spends nothing; the TCP listener binds
// port 0 so parallel cases and a live daemon never collide.
func writeCaseConfig(home, forgeBin, repoName, repoPath string) error {
	if err := os.MkdirAll(home, 0o700); err != nil {
		return fmt.Errorf("create case home: %w", err)
	}
	config := "[http]\nlisten = \"127.0.0.1:0\"\n"
	if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte(config), 0o600); err != nil {
		return fmt.Errorf("write config.toml: %w", err)
	}
	worker := fmt.Sprintf(`daemon = %q
token_file = %q
name = "eval"
max_concurrent = 1
data_dir = %q

[executors.claude-code]
command = [%q, "fake-claude", "--fixture", "{{fixture}}", "--model", "{{model}}", "--max-turns", "{{max_turns}}", "--mcp-config", "{{mcp_config}}"]
output = "claude-stream-json"
capabilities = ["allowed_tools", "json_schema", "resume"]
sandbox = false

[repositories.%s]
path = %q
base_branch = "main"
`, "unix://"+filepath.Join(home, "forge.sock"), filepath.Join(home, "token"), filepath.Join(home, "worker"), forgeBin, repoName, repoPath)
	if err := os.WriteFile(filepath.Join(home, "worker.toml"), []byte(worker), 0o600); err != nil {
		return fmt.Errorf("write worker.toml: %w", err)
	}
	return nil
}

// caseEnv builds the child environment: the parent's, minus every FORGE_*
// remnant that could leak state in and minus INVOCATION_ID — under a systemd
// user session the daemon would otherwise defer to `systemctl --user start
// forge-worker` and touch the operator's LIVE worker instead of spawning its
// own. The daemon's own pass-through list forwards FORGE_FAKE_FIXTURE to the
// worker and the worker's to the executor.
func caseEnv(home, fixture string) []string {
	var env []string
	for _, kv := range os.Environ() {
		key, _, ok := strings.Cut(kv, "=")
		if ok && (strings.HasPrefix(key, "FORGE_") || key == "INVOCATION_ID") {
			continue
		}
		env = append(env, kv)
	}
	return append(env,
		"FORGE_HOME="+home,
		"FORGE_FAKE_FIXTURE="+fixture,
		"FORGE_LOG_LEVEL=warn")
}

// startDaemon runs `forge daemon start --foreground` as an owned child and
// waits for its socket. stop tears it down (SIGTERM, with WaitDelay's kill as
// the backstop) and is safe to call once.
func startDaemon(ctx context.Context, forgeBin, home string, env []string) (func(), error) {
	if err := os.MkdirAll(filepath.Join(home, "logs"), 0o700); err != nil {
		return nil, fmt.Errorf("create logs dir: %w", err)
	}
	stdio, err := os.OpenFile(filepath.Join(home, "logs", "eval-daemon.stdio.log"), os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
	if err != nil {
		return nil, fmt.Errorf("open daemon stdio log: %w", err)
	}
	cmd := exec.CommandContext(ctx, forgeBin, "daemon", "start", "--foreground")
	cmd.Env = env
	cmd.Stdout, cmd.Stderr = stdio, stdio
	cmd.Cancel = func() error { return cmd.Process.Signal(syscall.SIGTERM) }
	cmd.WaitDelay = 10 * time.Second
	if err := cmd.Start(); err != nil {
		return nil, errors.Join(fmt.Errorf("start daemon: %w", err), stdio.Close())
	}
	if err := stdio.Close(); err != nil {
		return nil, err
	}
	stop := func() {
		if serr := cmd.Process.Signal(syscall.SIGTERM); serr != nil && !errors.Is(serr, os.ErrProcessDone) {
			if kerr := cmd.Process.Kill(); kerr != nil && !errors.Is(kerr, os.ErrProcessDone) {
				// Nothing left to do: the process is beyond signalling.
				return
			}
		}
		if werr := cmd.Wait(); werr != nil {
			// Expected: SIGTERM ends the daemon non-zero on some paths; the
			// stdio log has the story when a case needs diagnosing.
			return
		}
	}
	sock := filepath.Join(home, "forge.sock")
	deadline := time.Now().Add(15 * time.Second)
	for {
		conn, derr := (&net.Dialer{}).DialContext(ctx, "unix", sock)
		if derr == nil {
			if cerr := conn.Close(); cerr != nil {
				stop()
				return nil, cerr
			}
			return stop, nil
		}
		if time.Now().After(deadline) || ctx.Err() != nil {
			stop()
			tail := tailOf(filepath.Join(home, "logs", "eval-daemon.stdio.log"))
			return nil, fmt.Errorf("daemon socket %s did not answer: %w\n%s", sock, derr, tail)
		}
		time.Sleep(100 * time.Millisecond)
	}
}

// tailOf returns the last kilobyte of a log file for error messages.
func tailOf(path string) string {
	b, err := os.ReadFile(path)
	if err != nil {
		return ""
	}
	if len(b) > 1024 {
		b = b[len(b)-1024:]
	}
	return string(b)
}

// submitAndWait adds the case's task through the CLI and polls `task show`
// until the derived state is terminal.
func submitAndWait(ctx context.Context, o Options, c Case, home, repoName string, env []string) (*taskView, error) {
	args := []string{"task", "add", c.Prompt, "--repo", repoName, "--mode", c.Mode, "--model", o.Model, "--json"}
	if c.Autonomy != "" {
		args = append(args, "--autonomy", c.Autonomy)
	}
	out, err := runForge(ctx, o.ForgeBin, env, args...)
	if err != nil {
		return nil, fmt.Errorf("task add: %w", err)
	}
	var created taskView
	if err := json.Unmarshal(out, &created); err != nil {
		return nil, fmt.Errorf("task add output: %w: %s", err, bytes.TrimSpace(out))
	}
	if created.Work.ID == "" {
		return nil, fmt.Errorf("task add returned no work id: %s", bytes.TrimSpace(out))
	}
	for {
		out, err := runForge(ctx, o.ForgeBin, env, "task", "show", created.Work.ID, "--json")
		if err != nil {
			return nil, fmt.Errorf("task show: %w", err)
		}
		var v taskView
		if err := json.Unmarshal(out, &v); err != nil {
			return nil, fmt.Errorf("task show output: %w", err)
		}
		if terminalStates[v.State] {
			return &v, nil
		}
		select {
		case <-ctx.Done():
			return nil, fmt.Errorf("case timed out in state %q (home %s)", v.State, home)
		case <-time.After(250 * time.Millisecond):
		}
	}
}

// runForge runs one CLI invocation against the case's daemon and returns its
// stdout; stderr rides along in the error so a refusal is diagnosable.
func runForge(ctx context.Context, forgeBin string, env []string, args ...string) ([]byte, error) {
	cmd := exec.CommandContext(ctx, forgeBin, args...)
	cmd.Env = env
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("forge %s: %w: %s", strings.Join(args, " "), err, bytes.TrimSpace(stderr.Bytes()))
	}
	return stdout.Bytes(), nil
}
