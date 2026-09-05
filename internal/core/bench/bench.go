package bench

// The bench substrate shared by the CLI and the daemon's scheduler: spec
// files (bench/specs/<name>.md, frontmatter + objective) and the throwaway
// self-origin repository a run builds in.

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
)

type Spec struct {
	Size     string // S|M|L, default L
	Model    string // model alias for the plan (tasks inherit), default sonnet
	Autonomy string // default auto
	MaxTurns int    // plan-attempt turns, default 40 (the ad-hoc 30 is too tight)
	Timeout  int    // plan-attempt seconds, default 3600 (the ad-hoc 1800 timed out)
	Body     string
}

func LoadSpec(path string) (*Spec, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("spec: %w", err)
	}
	spec := &Spec{Size: "L", Model: "sonnet", Autonomy: "auto", MaxTurns: 40, Timeout: 3600, Body: string(raw)}
	body := string(raw)
	if strings.HasPrefix(body, "---\n") {
		end := strings.Index(body[4:], "\n---")
		if end < 0 {
			return nil, fmt.Errorf("spec %s: unterminated frontmatter", path)
		}
		for _, line := range strings.Split(body[4:4+end], "\n") {
			k, v, ok := strings.Cut(line, ":")
			if !ok {
				continue
			}
			v = strings.TrimSpace(v)
			switch strings.TrimSpace(k) {
			case "size":
				spec.Size = v
			case "model":
				spec.Model = v
			case "autonomy":
				spec.Autonomy = v
			case "max_turns":
				if n, err := strconv.Atoi(v); err == nil && n > 0 {
					spec.MaxTurns = n
				}
			case "timeout":
				if n, err := strconv.Atoi(v); err == nil && n > 0 {
					spec.Timeout = n
				}
			default:
				return nil, fmt.Errorf("spec %s: unknown key %q", path, strings.TrimSpace(k))
			}
		}
		spec.Body = strings.TrimSpace(strings.TrimPrefix(body[4+end:], "\n---\n"))
	}
	if strings.TrimSpace(spec.Body) == "" {
		return nil, fmt.Errorf("spec %s: empty objective", path)
	}
	return spec, nil
}

// initBenchRepo creates the throwaway checkout: git init on main, one empty
// commit, a self-referencing origin (repositories require one; the library
// repo set the precedent).
func InitRepo(ctx context.Context, path string) error {
	if _, err := os.Stat(path); err == nil {
		return fmt.Errorf("%s already exists", path)
	}
	if err := os.MkdirAll(path, 0o755); err != nil {
		return err
	}
	git := func(args ...string) error {
		out, err := exec.CommandContext(ctx, "git", append([]string{"-C", path}, args...)...).CombinedOutput()
		if err != nil {
			return fmt.Errorf("git %s: %s", strings.Join(args, " "), strings.TrimSpace(string(out)))
		}
		return nil
	}
	if err := git("init", "-q", "-b", "main"); err != nil {
		return err
	}
	// Constitution 10: the integrator refuses to push anywhere a repo has
	// not declared — found live when bench run 4's first green task landed
	// `conflict` on "no integration_branch". The throwaway declares its
	// branch from birth; checks arrive with the scaffold the plan builds.
	toml := "# Bench throwaway repo: integrate onto main; checks arrive with the scaffold.\nintegration_branch = \"main\"\n[checks]\n"
	if err := os.WriteFile(filepath.Join(path, "forge.toml"), []byte(toml), 0o644); err != nil {
		return err
	}
	if err := git("add", "forge.toml"); err != nil {
		return err
	}
	if err := git("-c", "user.name=forge", "-c", "user.email=forge@localhost", "commit", "-q", "-m", "bench: declare the integration branch"); err != nil {
		return err
	}
	if err := git("remote", "add", "origin", path); err != nil {
		return err
	}
	// The origin IS this checkout (repos must declare one); the integrator
	// pushes the integration branch to origin, and git refuses to update a
	// checked-out branch of a non-bare repo unless told to update the work
	// tree along with it.
	return git("config", "receive.denyCurrentBranch", "updateInstead")
}
