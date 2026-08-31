package controlplane

import (
	"testing"

	"forge/internal/model"
	"forge/internal/store"
)

// The conservative intersection rule (DESIGN.md §20): intersect unless
// provably disjoint. Every row is symmetric — the test checks both orders.
func TestGlobsIntersectPairs(t *testing.T) {
	cases := []struct {
		a, b      string
		intersect bool
	}{
		{"a/**", "b/**", false},
		{"a/**", "**", true},
		{"internal/*", "docs/**", false},
		{"internal/*", "internal/model/**", true}, // prefix relation: conservative
		{"internal/model/x.go", "internal/model/x.go", true},
		{"internal/model/x.go", "internal/model/y.go", false},
		{"internal", "internal/model/x.go", true}, // nested literals
		{"a", "ab", false},                        // sibling literals, no "/" boundary
		{"*.lock", "docs/**", false},              // single-segment vs directory glob
		{"*.lock", "Cargo.lock", true},
		{"*.lock", "go.mod", false},
		{"go.mod", "go.mod", true},
		{"*.lock", "**", true},
		{"go.*", "*.lock", true},      // two top-level patterns: conservative
		{"x.lock/**", "*.lock", true}, // the bare-directory case
		{"[bad", "anything/**", true}, // malformed glob: conservative
		{"cmd/forge/*.go", "ui/**", false},
	}
	for _, tc := range cases {
		for _, pair := range [][2]string{{tc.a, tc.b}, {tc.b, tc.a}} {
			if got := globsIntersect([]string{pair[0]}, []string{pair[1]}); got != tc.intersect {
				t.Errorf("globsIntersect(%q, %q) = %v, want %v", pair[0], pair[1], got, tc.intersect)
			}
		}
	}
	if !globsIntersect(nil, []string{"a/**"}) || !globsIntersect([]string{"a/**"}, nil) {
		t.Error("an empty set means the whole repository and intersects everything")
	}
}

// EffectiveGlobs: undeclared → whole repository; deps or lockfile-touching
// globs widen with the implicit exclusive lockfile lease.
func TestEffectiveGlobs(t *testing.T) {
	if got := EffectiveGlobs(nil, nil); len(got) != 1 || got[0] != "**" {
		t.Errorf("undeclared = %v, want [**]", got)
	}
	if got := EffectiveGlobs([]string{"docs/**"}, nil); len(got) != 1 {
		t.Errorf("docs-only must not widen: %v", got)
	}
	if got := EffectiveGlobs([]string{"docs/**"}, []string{"left-pad"}); len(got) != 1+len(LockfileGlobs) {
		t.Errorf("deps must widen with the lockfile lease: %v", got)
	}
	if got := EffectiveGlobs([]string{"go.sum"}, nil); len(got) != 1+len(LockfileGlobs) {
		t.Errorf("a lockfile-touching glob must widen: %v", got)
	}
	// Two deps-declaring works with disjoint code paths still conflict on the
	// lockfile lease — the whole point of the implicit lease.
	a := EffectiveGlobs([]string{"a/**"}, []string{"x"})
	b := EffectiveGlobs([]string{"b/**"}, []string{"y"})
	if !globsIntersect(a, b) {
		t.Error("two deps-declaring works must serialise on the lockfile lease")
	}
}

func TestPathMatchesGlob(t *testing.T) {
	cases := []struct {
		glob, p string
		want    bool
	}{
		{"**", "any/thing.go", true},
		{"docs/**", "docs/a/b.md", true},
		{"docs/**", "docs", true},
		{"docs/**", "src/a.go", false},
		{"*.lock", "Cargo.lock", true},
		{"*.lock", "sub/Cargo.lock", false},
		{"internal/*", "internal/x.go", true},
		{"internal/*", "internal/deep/x.go", false},
	}
	for _, tc := range cases {
		if got := PathMatchesGlob(tc.glob, tc.p); got != tc.want {
			t.Errorf("PathMatchesGlob(%q, %q) = %v, want %v", tc.glob, tc.p, got, tc.want)
		}
	}
}

func TestStackDepth(t *testing.T) {
	edges := []model.Edge{
		{Work: "b", BlockedBy: "a", On: model.OnSuccess, StackOn: true},
		{Work: "c", BlockedBy: "b", On: model.OnSuccess, StackOn: true},
		{Work: "d", BlockedBy: "c", On: model.OnSuccess}, // plain edge: no depth
	}
	for id, want := range map[string]int{"a": 0, "b": 1, "c": 2, "d": 0} {
		if got := StackDepth(id, edges); got != want {
			t.Errorf("StackDepth(%s) = %d, want %d", id, got, want)
		}
	}
}

// Pick refuses a stack deeper than max_stack_depth and names the reason.
func TestPickStackDepthCap(t *testing.T) {
	w := store.Work{ID: "c", Priority: 50, BudgetClass: model.ClassNormal}
	targets := map[string][]store.Target{"c": {{ID: "tc", WorkID: "c", Repository: "equitizr", State: model.Pending}}}
	edges := []model.Edge{
		{Work: "b", BlockedBy: "a", On: model.OnSuccess, StackOn: true},
		{Work: "c", BlockedBy: "b", On: model.OnSuccess, StackOn: true},
	}
	worker := store.Worker{ID: "w1"}
	repos := map[string]store.Repository{"equitizr": {Name: "equitizr", WorkerID: "w1"}}
	order := Order(QueueInput{Work: []store.Work{w}, Targets: targets})
	pick := Pick(PickInput{Order: order, Worker: worker, Repositories: repos, Edges: edges, MaxStackDepth: 1})
	if pick.Target != nil || pick.Skipped["tc"] != "stack depth 2 exceeds max_stack_depth 1" {
		t.Errorf("depth cap: %+v", pick)
	}
	pick = Pick(PickInput{Order: order, Worker: worker, Repositories: repos, Edges: edges, MaxStackDepth: 2})
	if pick.Target == nil {
		t.Errorf("depth 2 at cap 2 must be admitted: %+v", pick)
	}
}

// A lease-exempt Work (a non-writing mode) is neither blocked by a lease nor
// counted as holding one.
func TestPickLeaseExempt(t *testing.T) {
	w := store.Work{ID: "v", Priority: 50, BudgetClass: model.ClassNormal}
	targets := map[string][]store.Target{"v": {{ID: "tv", WorkID: "v", Repository: "equitizr", State: model.Pending}}}
	worker := store.Worker{ID: "w1"}
	repos := map[string]store.Repository{"equitizr": {Name: "equitizr", WorkerID: "w1"}}
	leases := []PathLease{{TargetID: "held", Repository: "equitizr", Globs: []string{"**"}}}
	order := Order(QueueInput{Work: []store.Work{w}, Targets: targets})
	pick := Pick(PickInput{Order: order, Worker: worker, Repositories: repos, Leases: leases})
	if pick.Target != nil {
		t.Fatalf("a writing work must wait on the whole-repo lease: %+v", pick)
	}
	pick = Pick(PickInput{Order: order, Worker: worker, Repositories: repos, Leases: leases, LeaseExempt: func(store.Work) bool { return true }})
	if pick.Target == nil {
		t.Fatalf("a lease-exempt work must run through a held lease: %+v", pick)
	}
}

// The queue annotates a fully lease-blocked pending Work with path_lease.
func TestOrderLeaseReason(t *testing.T) {
	w := store.Work{ID: "b", Priority: 50, BudgetClass: model.ClassNormal, Paths: []string{"internal/**"}}
	free := store.Work{ID: "f", Priority: 50, BudgetClass: model.ClassNormal, Paths: []string{"docs/**"}}
	targets := map[string][]store.Target{
		"b": {{ID: "tb", WorkID: "b", Repository: "equitizr", State: model.Pending}},
		"f": {{ID: "tf", WorkID: "f", Repository: "equitizr", State: model.Pending}},
	}
	leases := []PathLease{{TargetID: "0123456789abcdef0123456789abcdef", Repository: "equitizr", Globs: []string{"internal/**"}}}
	order := Order(QueueInput{Work: []store.Work{w, free}, Targets: targets, Leases: leases})
	byID := map[string]QueueEntry{}
	for _, e := range order {
		byID[e.Work.ID] = e
	}
	if byID["b"].State != model.WorkPending || byID["b"].Reason != "path_lease held by 01234567" {
		t.Errorf("blocked entry = %v/%q", byID["b"].State, byID["b"].Reason)
	}
	if byID["f"].Reason != "" {
		t.Errorf("disjoint entry must carry no reason, got %q", byID["f"].Reason)
	}
}
