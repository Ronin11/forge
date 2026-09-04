package tui

import (
	"context"
	"fmt"
	"os/exec"
	"path/filepath"
	"strings"

	"forge/internal/core/config"
)

// RunDirectives is `forge directives update`: pull base-library improvements
// from the upstream remote into the user's fork of ~/.forge/directives. The
// merge is an ordinary git merge — a conflict is left for the user to resolve
// in the library repo like any other; the daemon keeps serving its last good
// load throughout and picks the merged tree up within 30s.
func RunDirectives(ctx context.Context, c *Context, args []string) int {
	if len(args) == 0 || args[0] != "update" {
		fmt.Fprintln(c.Stderr, "usage: forge directives update")
		return 2
	}
	fs, _ := c.Flags("directives update")
	if code := c.Parse(fs, args[1:]); code >= 0 {
		return code
	}
	cfg, err := config.LoadConfig(filepath.Join(c.ForgeHome, "config.toml"), c.ForgeHome, c.UserHome, c.Getenv)
	if err != nil {
		return c.Fail("directives", err)
	}
	dir := cfg.Prompts.Path
	git := func(args ...string) (string, error) {
		out, err := exec.CommandContext(ctx, "git", append([]string{"-C", dir}, args...)...).CombinedOutput()
		return strings.TrimSpace(string(out)), err
	}
	if out, err := git("remote", "get-url", "upstream"); err != nil {
		return c.Fail("directives update", fmt.Errorf("no upstream remote in %s (%s) — add one: git -C %s remote add upstream <url>", dir, out, dir))
	}
	if out, err := git("fetch", "upstream"); err != nil {
		return c.Fail("directives update", fmt.Errorf("fetch: %s", out))
	}
	out, err := git("merge", "--no-edit", "FETCH_HEAD")
	if err != nil {
		fmt.Fprintln(c.Stdout, out)
		return c.Fail("directives update", fmt.Errorf("merge conflict — resolve it in %s (git status there), then commit; the daemon keeps its last good load meanwhile", dir))
	}
	fmt.Fprintln(c.Stdout, out)
	fmt.Fprintln(c.Stdout, "updated — the daemon reloads within 30s; new seeds import on its next boot")
	return 0
}
