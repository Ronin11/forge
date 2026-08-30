package controlplane

import (
	"sort"

	"forge/internal/model"
	"forge/internal/store"
)

// QueueEntry is one Work in the priority queue with its derived state and, when
// it cannot run, the reason.
type QueueEntry struct {
	Work     store.Work
	Targets  []store.Target
	State    model.WorkState
	Reason   string   // for blocked/deferred
	Waiting  []string // dependency ids not yet satisfied
	FailedOn []string // success dependencies that failed
	Position int
}

// QueueInput is what Order needs: every open Work with its targets and edges,
// and the budget decision per class.
type QueueInput struct {
	Work     []store.Work
	Targets  map[string][]store.Target
	Edges    []model.Edge
	Deferred func(class model.BudgetClass) (deferred bool, reason string)
	// States of finished dependencies not in Work (looked up by the caller).
	FinishedStates map[string]model.WorkState
}

// Order is the one home of queue order and eligibility: priority desc, class
// rank, created_at asc, with each entry's derived state and block/defer reason.
func Order(in QueueInput) []QueueEntry {
	entries := make([]QueueEntry, 0, len(in.Work))
	states := map[string]model.WorkState{}
	for id, s := range in.FinishedStates {
		states[id] = s
	}
	// First pass: derive states without dependency/budget effects so dependency
	// evaluation sees siblings' states.
	edgesByWork := map[string][]model.Edge{}
	for _, e := range in.Edges {
		edgesByWork[e.Work] = append(edgesByWork[e.Work], e)
	}
	for _, w := range in.Work {
		ts := targetStates(in.Targets[w.ID])
		states[w.ID] = model.DeriveWorkState(model.WorkInputs{Targets: ts, Integrate: w.Integrate})
	}
	for _, w := range in.Work {
		e := QueueEntry{Work: w, Targets: in.Targets[w.ID]}
		deps := model.Dependencies(edgesByWork[w.ID], states)
		blocked := !deps.Satisfied
		deferred, reason := false, ""
		if in.Deferred != nil && !blocked {
			deferred, reason = in.Deferred(w.BudgetClass)
		}
		e.State = model.DeriveWorkState(model.WorkInputs{Targets: targetStates(e.Targets), Integrate: w.Integrate, Blocked: blocked, Deferred: deferred})
		switch {
		case e.State == model.WorkBlocked && len(deps.FailedDeps) > 0:
			e.Reason, e.FailedOn, e.Waiting = "dependency_failed", deps.FailedDeps, deps.Waiting
		case e.State == model.WorkBlocked:
			e.Reason, e.Waiting = "waiting_on_dependencies", deps.Waiting
		case e.State == model.WorkDeferred:
			e.Reason = reason
		}
		entries = append(entries, e)
	}
	sort.SliceStable(entries, func(i, j int) bool {
		a, b := entries[i].Work, entries[j].Work
		if a.Priority != b.Priority {
			return a.Priority > b.Priority
		}
		if a.BudgetClass.Rank() != b.BudgetClass.Rank() {
			return a.BudgetClass.Rank() < b.BudgetClass.Rank()
		}
		return a.CreatedAt.Before(b.CreatedAt)
	})
	for i := range entries {
		entries[i].Position = i
	}
	return entries
}

func targetStates(ts []store.Target) []model.State {
	out := make([]model.State, len(ts))
	for i, t := range ts {
		out[i] = t.State
	}
	return out
}

// Violates reports whether placing `moving` immediately before `before` would
// put a Work above one it is blocked by (the drag-and-drop rule, enforced by the
// API too).
func Violates(order []QueueEntry, edges []model.Edge, moving, before string) bool {
	blockedBy := map[string]map[string]bool{}
	for _, e := range edges {
		if blockedBy[e.Work] == nil {
			blockedBy[e.Work] = map[string]bool{}
		}
		blockedBy[e.Work][e.BlockedBy] = true
	}
	// Positions after the move.
	ids := make([]string, 0, len(order))
	for _, e := range order {
		if e.Work.ID != moving {
			ids = append(ids, e.Work.ID)
		}
	}
	out := make([]string, 0, len(ids)+1)
	placed := false
	for _, id := range ids {
		if id == before {
			out = append(out, moving)
			placed = true
		}
		out = append(out, id)
	}
	if !placed {
		out = append(out, moving)
	}
	pos := map[string]int{}
	for i, id := range out {
		pos[id] = i
	}
	for work, deps := range blockedBy {
		for dep := range deps {
			if pw, ok := pos[work]; ok {
				if pd, ok := pos[dep]; ok && pw < pd {
					return true
				}
			}
		}
	}
	return false
}
