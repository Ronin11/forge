// Package model holds the Target state machine and the derived Work state.
// It is the only place a Target state may change.
package model

import "fmt"

// State is the state of one Target.
type State string

const (
	Pending   State = "pending"
	Claimed   State = "claimed"
	Preparing State = "preparing"
	Running   State = "running"
	Succeeded State = "succeeded"
	Failed    State = "failed"
	Cancelled State = "cancelled"
)

// Work states that are not Target states.
const (
	WorkPartial State = "partial"
)

var transitions = map[State][]State{
	Pending:   {Claimed, Cancelled},
	Claimed:   {Preparing, Failed, Cancelled},
	Preparing: {Running, Failed, Cancelled},
	Running:   {Succeeded, Failed, Cancelled},
}

// Valid reports whether s names a Target state.
func Valid(s State) bool {
	switch s {
	case Pending, Claimed, Preparing, Running, Succeeded, Failed, Cancelled:
		return true
	}
	return false
}

// Terminal reports whether no further transition is possible.
func Terminal(s State) bool {
	return s == Succeeded || s == Failed || s == Cancelled
}

// Active reports whether a worker currently holds the Target.
func Active(s State) bool {
	return s == Claimed || s == Preparing || s == Running
}

// Transition validates from → to and returns an error for every illegal edge.
func Transition(from, to State) error {
	if !Valid(from) || !Valid(to) {
		return fmt.Errorf("unknown target state in transition %q -> %q", from, to)
	}
	for _, allowed := range transitions[from] {
		if allowed == to {
			return nil
		}
	}
	return fmt.Errorf("illegal target transition %q -> %q", from, to)
}

// WorkState derives the aggregate state of a Work from its Target states.
// Precedence follows the Factory design: terminal mixes first, then running,
// then pending. An empty Work is failed (it can never make progress).
func WorkState(targets []State) State {
	if len(targets) == 0 {
		return Failed
	}
	var succeeded, failed, cancelled, active, pending int
	for _, s := range targets {
		switch {
		case s == Succeeded:
			succeeded++
		case s == Failed:
			failed++
		case s == Cancelled:
			cancelled++
		case Active(s):
			active++
		default:
			pending++
		}
	}
	terminal := succeeded + failed + cancelled
	if terminal == len(targets) {
		switch {
		case succeeded == terminal:
			return Succeeded
		case cancelled == terminal:
			return Cancelled
		case succeeded == 0 && failed > 0 && cancelled == 0:
			return Failed
		default:
			return WorkPartial
		}
	}
	if active > 0 || terminal > 0 {
		return Running
	}
	return Pending
}
