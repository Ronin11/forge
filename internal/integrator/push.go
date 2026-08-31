package integrator

import (
	"context"
	"errors"
	"fmt"
	"path"
	"strings"

	"forge/internal/worker"
)

// ErrPushRefused wraps every push-policy refusal (constitution 10): callers
// journal it and land the Target in conflict rather than pushing anywhere.
var ErrPushRefused = errors.New("push refused by policy")

// AllowedPushBranch is the push policy's branch rule (constitution 10): Forge
// pushes only to a branch the repository's forge.toml lists —
// integration_branch exactly, or a task_branches glob match. Everything else
// is refused, including a repository that declares nothing.
func AllowedPushBranch(branch string, ft *worker.ForgeToml) error {
	if branch == "" || strings.HasPrefix(branch, "-") || strings.ContainsAny(branch, "+:~^ \t") {
		return fmt.Errorf("%w: branch %q is not a plain branch name", ErrPushRefused, branch)
	}
	if ft == nil || ft.IntegrationBranch == "" {
		return fmt.Errorf("%w: forge.toml declares no integration_branch", ErrPushRefused)
	}
	if branch == ft.IntegrationBranch {
		return nil
	}
	if ft.TaskBranches != "" {
		if ok, err := path.Match(ft.TaskBranches, branch); err == nil && ok {
			return nil
		}
	}
	return fmt.Errorf("%w: branch %q is not integration_branch %q or task_branches %q", ErrPushRefused, branch, ft.IntegrationBranch, ft.TaskBranches)
}

// PushIntegration is the ONE place Forge pushes (constitution 10). It runs
// from the integrator's scratch clone, daemon-side and outside any sandbox,
// only after the repository's declared checks passed on the rebased result.
// The invocation is fixed by construction: no force flag of any kind and no
// "+" in the refspec can ever appear — a test greps this file to prove it —
// and git's default fast-forward rule rejects a non-ff push at the remote.
// It never deletes remote refs (a delete would need an empty or ":"-prefixed
// refspec, which AllowedPushBranch's branch validation excludes).
func PushIntegration(ctx context.Context, g worker.Git, dir, remoteURL, branch string, ft *worker.ForgeToml) error {
	if err := AllowedPushBranch(branch, ft); err != nil {
		return err
	}
	if remoteURL == "" {
		return fmt.Errorf("%w: no remote URL", ErrPushRefused)
	}
	if _, err := g.Run(ctx, dir, "push", remoteURL, "HEAD:refs/heads/"+branch); err != nil {
		return fmt.Errorf("push %s: %w", branch, err)
	}
	return nil
}
