package model

// DependencyOn says when a blocked_by edge is satisfied.
type DependencyOn string

const (
	OnSuccess  DependencyOn = "success"
	OnTerminal DependencyOn = "terminal"
)

// Edge is one blocked_by relation: Work depends on BlockedBy. StackOn lets an
// on:success dependant start on the dependency's branch head once its agent work
// succeeded, before it is merged (DESIGN.md §20).
type Edge struct {
	Work      string
	BlockedBy string
	On        DependencyOn
	StackOn   bool
}

// WouldCycle reports whether adding candidate to edges creates a cycle. Edges of
// terminal Work may be omitted by the caller; they cannot participate in a cycle
// that matters.
func WouldCycle(edges []Edge, candidate Edge) bool {
	if candidate.Work == candidate.BlockedBy {
		return true
	}
	next := map[string][]string{}
	for _, e := range edges {
		next[e.Work] = append(next[e.Work], e.BlockedBy)
	}
	next[candidate.Work] = append(next[candidate.Work], candidate.BlockedBy)
	// A cycle through the candidate exists iff candidate.Work is reachable from
	// candidate.BlockedBy along blocked_by edges.
	seen := map[string]bool{}
	stack := []string{candidate.BlockedBy}
	for len(stack) > 0 {
		n := stack[len(stack)-1]
		stack = stack[:len(stack)-1]
		if n == candidate.Work {
			return true
		}
		if seen[n] {
			continue
		}
		seen[n] = true
		stack = append(stack, next[n]...)
	}
	return false
}

// DependencyStatus is the scheduler's view of one Work's edges.
type DependencyStatus struct {
	Satisfied bool
	// Waiting lists dependencies not yet satisfied; FailedDeps lists success
	// dependencies that ended any other way (the Work is blocked until a human acts).
	Waiting    []string
	FailedDeps []string
}

// Dependencies evaluates a Work's edges given each dependency's derived state.
// A missing state (deleted Work) counts as failed: better blocked than silently
// admitted.
func Dependencies(edges []Edge, states map[string]WorkState) DependencyStatus {
	out := DependencyStatus{Satisfied: true}
	for _, e := range edges {
		st, ok := states[e.BlockedBy]
		switch {
		case !ok:
			out.FailedDeps = append(out.FailedDeps, e.BlockedBy)
		case e.On == OnSuccess && (st == WorkSucceeded || st == WorkMerged):
		case e.On == OnSuccess && e.StackOn && st == WorkMerging:
		case e.On == OnTerminal && workTerminal(st):
		case e.On == OnSuccess && workTerminal(st):
			out.FailedDeps = append(out.FailedDeps, e.BlockedBy)
		default:
			out.Waiting = append(out.Waiting, e.BlockedBy)
		}
	}
	out.Satisfied = len(out.Waiting) == 0 && len(out.FailedDeps) == 0
	return out
}

func workTerminal(s WorkState) bool {
	switch s {
	case WorkSucceeded, WorkUnverified, WorkFailed, WorkCancelled, WorkPartial, WorkMerged:
		return true
	}
	return false
}
