package worker

import "fmt"

// CleanupInput is everything the cleanup decision depends on (DESIGN.md §6).
type CleanupInput struct {
	Resumable      bool // waiting_human: the session will continue here
	AwaitingMerge  bool // integrating Work not yet merged
	Greenfield     bool // the directory is the product
	PathExists     bool
	Registered     bool // git worktree list knows the path
	Dirty          bool
	HeadIsBase     bool
	Pushed         bool // head reachable from a remote ref
	AttemptShortID string
}

// CleanupAction is what the worker does next.
type CleanupAction string

const (
	CleanupKeep    CleanupAction = "keep"    // nothing to do; not a retention
	CleanupRemove  CleanupAction = "remove"  // git worktree remove (never --force)
	CleanupRetain  CleanupAction = "retain"  // keep and report with a reason
	CleanupMissing CleanupAction = "missing" // nothing to remove
)

// CleanupDecision is the outcome; Command is the exact operator command for a
// retained worktree, Inconsistent flags a filesystem/registry disagreement.
type CleanupDecision struct {
	Action       CleanupAction
	Reason       string
	Command      string
	Inconsistent bool
}

// DecideCleanup is the one home of the retain/remove rule. First match wins;
// the table is DESIGN.md §6 verbatim. The branch is never part of the decision
// because Forge never deletes branches.
func DecideCleanup(in CleanupInput) CleanupDecision {
	cmd := fmt.Sprintf("forge cleanup %s --confirm", in.AttemptShortID)
	switch {
	case in.Resumable:
		return CleanupDecision{Action: CleanupKeep, Reason: "awaiting human answer"}
	case in.AwaitingMerge:
		return CleanupDecision{Action: CleanupKeep, Reason: "awaiting merge"}
	case in.Greenfield:
		return CleanupDecision{Action: CleanupKeep, Reason: "greenfield project"}
	case !in.PathExists && !in.Registered:
		return CleanupDecision{Action: CleanupMissing, Reason: "worktree missing"}
	case in.PathExists != in.Registered:
		return CleanupDecision{Action: CleanupRetain, Reason: "worktree exists in only one of filesystem and git registry", Command: cmd, Inconsistent: true}
	case in.Dirty:
		return CleanupDecision{Action: CleanupRetain, Reason: "dirty worktree", Command: cmd}
	case !in.HeadIsBase && !in.Pushed:
		return CleanupDecision{Action: CleanupRetain, Reason: "unpushed commits", Command: cmd}
	default:
		return CleanupDecision{Action: CleanupRemove, Reason: "removed"}
	}
}
