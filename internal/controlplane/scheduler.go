package controlplane

import (
	"fmt"
	"path"
	"sort"

	"forge/internal/model"
	"forge/internal/store"
)

// SchedulerPolicy decides, per budget class, whether new admissions are allowed
// right now. M1 ships AdmitAll; M3 replaces it with the budget policy behind the
// same interface.
type SchedulerPolicy interface {
	Decide(class model.BudgetClass) (admit bool, reason string)
}

// AdmitAll is the M1 policy: everything is admissible.
type AdmitAll struct{}

// Decide always admits.
func (AdmitAll) Decide(model.BudgetClass) (bool, string) { return true, "" }

// PickInput is the claim transaction's view: the queue order, the workers, the
// routines' concurrency and active counts, live path leases, and the worker
// asking. Everything is read inside the same transaction as the claim.
type PickInput struct {
	Order          []QueueEntry
	Worker         store.Worker
	Repositories   map[string]store.Repository // by name; WorkerID says who advertises it
	Concurrency    map[string]int              // routine id → limit
	Active         map[string]int              // routine id → active Work
	Leases         []PathLease                 // live path leases per repository
	Requirements   func(w store.Work) []string // capabilities a Work needs (sandbox, browser, …)
	RunnerCapacity func(model string) (free bool, reason string)
}

// PathLease is one live write-set lease.
type PathLease struct {
	TargetID   string
	Repository string
	Globs      []string
}

// Pick returns the first Target the worker may claim, or nil with a summary of
// why each candidate was skipped (for the queue page and logs).
type PickResult struct {
	Target  *store.Target
	Work    *store.Work
	Skipped map[string]string // target id → reason
}

// Pick evaluates the admission rules of DESIGN.md §10.2 in order for every
// pending Target in queue order.
func Pick(in PickInput) PickResult {
	res := PickResult{Skipped: map[string]string{}}
	for _, e := range in.Order {
		if e.State != model.WorkPending && e.State != model.WorkRunning && e.State != model.WorkWaitingHuman {
			continue
		}
		for i := range e.Targets {
			t := e.Targets[i]
			if t.State != model.Pending {
				continue
			}
			if reason := skipReason(in, e, t); reason != "" {
				res.Skipped[t.ID] = reason
				continue
			}
			w := e.Work
			res.Target, res.Work = &t, &w
			return res
		}
	}
	return res
}

func skipReason(in PickInput, e QueueEntry, t store.Target) string {
	w := e.Work
	if t.WorkerID != "" && t.WorkerID != in.Worker.ID {
		return "pinned to worker " + t.WorkerID
	}
	if w.RoutineID != "" {
		if limit, ok := in.Concurrency[w.RoutineID]; ok && in.Active[w.RoutineID] >= limit {
			return fmt.Sprintf("routine concurrency %d reached", limit)
		}
	}
	repo, ok := in.Repositories[t.Repository]
	if !ok || repo.WorkerID != in.Worker.ID {
		return "repository " + t.Repository + " not advertised by this worker"
	}
	var snapshot struct {
		Executor string `json:"executor"`
	}
	_ = snapshot
	if in.Requirements != nil {
		for _, cap := range in.Requirements(w) {
			if in.Worker.Capabilities[cap] != "ready" {
				return "worker lacks capability " + cap
			}
		}
	}
	if in.RunnerCapacity != nil {
		if free, reason := in.RunnerCapacity(""); !free {
			return reason
		}
	}
	for _, l := range in.Leases {
		if l.Repository != t.Repository {
			continue
		}
		if globsIntersect(w.Paths, l.Globs) {
			return "path_lease held by " + l.TargetID
		}
	}
	return ""
}

// globsIntersect is conservative: an undeclared write set means the whole
// repository, and two globs intersect unless they are both literal paths that
// differ or share no prefix relation. M9 refines this; the rule's home is here.
func globsIntersect(a, b []string) bool {
	if len(a) == 0 || len(b) == 0 {
		return true
	}
	for _, x := range a {
		for _, y := range b {
			if x == y || matchesEither(x, y) {
				return true
			}
		}
	}
	return false
}

// matchesEither treats a malformed glob as non-matching; globs are validated
// where they enter (task add, routine save), so this is belt and braces.
func matchesEither(x, y string) bool {
	if ok, err := path.Match(x, y); err == nil && ok {
		return true
	}
	if ok, err := path.Match(y, x); err == nil && ok {
		return true
	}
	return false
}

// SortedSkips renders the skip map deterministically for logs.
func SortedSkips(m map[string]string) []string {
	out := make([]string, 0, len(m))
	for id, r := range m {
		out = append(out, id[:8]+": "+r)
	}
	sort.Strings(out)
	return out
}
