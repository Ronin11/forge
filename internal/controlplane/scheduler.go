package controlplane

import (
	"fmt"
	"path"
	"sort"
	"strings"

	"forge/internal/core/model"
	"forge/internal/core/store"
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
	// Edges and MaxStackDepth gate stacking (DESIGN.md §20): a Work whose
	// stack_on chain is deeper than the cap is skipped. Zero means default 2.
	Edges         []model.Edge
	MaxStackDepth int
	// LeaseExempt reports Work whose mode writes nothing to the repository
	// (verify, plan): it neither takes nor is blocked by a path lease — a
	// write-set lease guards writes, and exempting readers is what lets a
	// verify follow-up run while its subject holds the lease in verifying.
	LeaseExempt func(w store.Work) bool
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
	// A paused repository admits no new work; running attempts continue, the
	// same posture as a budget stop (DESIGN.md §10.2). The paused flag rides on
	// the Repository row Repositories() already returned, so no extra read.
	if repo.Paused {
		return "repository_paused"
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
	if in.LeaseExempt == nil || !in.LeaseExempt(w) {
		globs := EffectiveGlobs(w.Paths, w.Deps)
		for _, l := range in.Leases {
			if l.Repository != t.Repository {
				continue
			}
			if globsIntersect(globs, l.Globs) {
				return "path_lease held by " + l.TargetID
			}
		}
	}
	if len(in.Edges) > 0 {
		limit := in.MaxStackDepth
		if limit <= 0 {
			limit = 2
		}
		if d := StackDepth(w.ID, in.Edges); d > limit {
			return fmt.Sprintf("stack depth %d exceeds max_stack_depth %d", d, limit)
		}
	}
	return ""
}

// LockfileGlobs is the implicit exclusive lease (DESIGN.md §20): dependency
// metadata is a single shared resource per repository, so any Work that could
// touch it serialises against any other such Work.
var LockfileGlobs = []string{"go.mod", "go.sum", "package.json", "*.lock"}

// EffectiveGlobs is the write set the scheduler leases for a Work: the
// declared globs, widened with LockfileGlobs when the Work declares deps or
// any declared glob could touch a lockfile. Undeclared paths mean the whole
// repository — ["**"] — so two undeclared Works on one repository serialise
// (DESIGN.md §20).
func EffectiveGlobs(paths, deps []string) []string {
	if len(paths) == 0 {
		return []string{"**"}
	}
	widen := len(deps) > 0
	if !widen {
	outer:
		for _, p := range paths {
			for _, l := range LockfileGlobs {
				if globPairIntersects(p, l) {
					widen = true
					break outer
				}
			}
		}
	}
	if !widen {
		return paths
	}
	out := make([]string, 0, len(paths)+len(LockfileGlobs))
	out = append(out, paths...)
	out = append(out, LockfileGlobs...)
	return out
}

// globsIntersect is conservative (DESIGN.md §20): an empty set means the whole
// repository, and two globs intersect unless provably disjoint. The rule's
// home is here.
func globsIntersect(a, b []string) bool {
	if len(a) == 0 || len(b) == 0 {
		return true
	}
	for _, x := range a {
		for _, y := range b {
			if globPairIntersects(x, y) {
				return true
			}
		}
	}
	return false
}

// globPairIntersects decides one pair. Two literal paths intersect only when
// equal or nested; anything else intersects unless the literal prefixes (up
// to the first metacharacter) diverge — so "a/**" and "b/**" are disjoint,
// anything against "**" intersects, and a malformed glob is simply treated
// as its literal prefix (conservative, never an error).
func globPairIntersects(x, y string) bool {
	px, litX := literalPrefix(x)
	py, litY := literalPrefix(y)
	if litX && litY {
		return x == y || strings.HasPrefix(x, y+"/") || strings.HasPrefix(y, x+"/")
	}
	// A single-segment glob ("*.lock", "go.*") matches only top-level names
	// (path.Match's * never crosses a slash): it can meet a slash-carrying
	// glob only at that glob's bare first directory ("docs/**" matches
	// "docs" itself).
	if x != "**" && !strings.Contains(x, "/") {
		return topLevelMeets(x, y)
	}
	if y != "**" && !strings.Contains(y, "/") {
		return topLevelMeets(y, x)
	}
	return strings.HasPrefix(px, py) || strings.HasPrefix(py, px)
}

// topLevelMeets decides whether the single-segment glob seg can share a path
// with other. Conservative: any case it cannot prove disjoint intersects.
func topLevelMeets(seg, other string) bool {
	if other == "**" {
		return true
	}
	first, _, hasSlash := strings.Cut(other, "/")
	if !hasSlash {
		// Two top-level entries: a literal on either side is decidable.
		if _, lit := literalPrefix(other); lit {
			return matchGlob(seg, other)
		}
		if _, lit := literalPrefix(seg); lit {
			return matchGlob(other, seg)
		}
		return true
	}
	if _, lit := literalPrefix(first); !lit {
		return true
	}
	return matchGlob(seg, first)
}

// matchGlob is path.Match with a malformed pattern treated as MATCHING: the
// intersection rule is conservative, and a glob nobody can parse must not
// slip past the lease.
func matchGlob(glob, name string) bool {
	ok, err := path.Match(glob, name)
	return ok || err != nil
}

// literalPrefix returns the glob's leading literal bytes before the first
// metacharacter and whether the whole glob is literal.
func literalPrefix(g string) (string, bool) {
	if i := strings.IndexAny(g, "*?["); i >= 0 {
		return g[:i], false
	}
	return g, true
}

// PathMatchesGlob reports whether one concrete path is inside one glob, with
// the same dialect the docs write-scope uses: "**" is everything, a "/**"
// suffix is a directory prefix, anything else is path.Match (which never
// crosses a slash). write_set_precision (facts) uses this matcher.
func PathMatchesGlob(glob, p string) bool {
	if glob == "**" {
		return true
	}
	if prefix, ok := strings.CutSuffix(glob, "/**"); ok {
		return p == prefix || strings.HasPrefix(p, prefix+"/")
	}
	ok, err := path.Match(glob, p)
	return err == nil && ok
}

// StackDepth is the length of the stack_on chain below a Work: 0 for an
// unstacked Work, 1 when it stacks on an unstacked one, and so on. A cycle
// (impossible by construction — CreateWork refuses them) reports the depth
// walked so far rather than looping.
func StackDepth(workID string, edges []model.Edge) int {
	stackedOn := map[string]string{}
	for _, e := range edges {
		if e.StackOn && e.On == model.OnSuccess {
			stackedOn[e.Work] = e.BlockedBy
		}
	}
	depth := 0
	seen := map[string]bool{}
	for cur := workID; !seen[cur]; {
		seen[cur] = true
		next, ok := stackedOn[cur]
		if !ok {
			return depth
		}
		depth++
		cur = next
	}
	return depth
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
