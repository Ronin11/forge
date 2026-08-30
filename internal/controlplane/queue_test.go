package controlplane

import (
	"testing"
	"time"

	"forge/internal/model"
	"forge/internal/store"
)

func TestOrderAndPick(t *testing.T) {
	t0 := time.Date(2026, 8, 30, 12, 0, 0, 0, time.UTC)
	mk := func(id string, prio int, class model.BudgetClass, created time.Duration, routine string) store.Work {
		return store.Work{ID: id, Priority: prio, BudgetClass: class, CreatedAt: t0.Add(created), RoutineID: routine, RoutineName: "r"}
	}
	a := mk("a", 50, model.ClassBacklog, 0, "r1")
	b := mk("b", 50, model.ClassNormal, time.Second, "r1")
	c := mk("c", 100, model.ClassInteractive, 2*time.Second, "")
	d := mk("d", 50, model.ClassNormal, 3*time.Second, "r2")
	targets := map[string][]store.Target{
		"a": {{ID: "ta", WorkID: "a", Repository: "equitizr", State: model.Pending}},
		"b": {{ID: "tb", WorkID: "b", Repository: "equitizr", State: model.Pending}},
		"c": {{ID: "tc", WorkID: "c", Repository: "equitizr", State: model.Pending}},
		"d": {{ID: "td", WorkID: "d", Repository: "other", State: model.Pending}},
	}
	edges := []model.Edge{{Work: "b", BlockedBy: "a", On: model.OnSuccess}}
	deferBacklog := func(class model.BudgetClass) (bool, string) {
		return class == model.ClassBacklog, "ahead_of_burn_down_line"
	}
	order := Order(QueueInput{Work: []store.Work{a, b, c, d}, Targets: targets, Edges: edges, Deferred: deferBacklog})
	ids := []string{}
	for _, e := range order {
		ids = append(ids, e.Work.ID)
	}
	if got := ids[0] + ids[1] + ids[2] + ids[3]; got != "cbda" && got != "cbad" {
		// b (normal) outranks a (backlog) at equal priority; d is normal but newer than b.
		t.Errorf("order = %v", ids)
	}
	byID := map[string]QueueEntry{}
	for _, e := range order {
		byID[e.Work.ID] = e
	}
	if byID["c"].State != model.WorkPending || byID["a"].State != model.WorkDeferred || byID["a"].Reason != "ahead_of_burn_down_line" {
		t.Errorf("c=%v a=%v/%s", byID["c"].State, byID["a"].State, byID["a"].Reason)
	}
	if byID["b"].State != model.WorkBlocked || byID["b"].Reason != "waiting_on_dependencies" || byID["b"].Waiting[0] != "a" {
		t.Errorf("b = %+v", byID["b"])
	}
	failed := Order(QueueInput{Work: []store.Work{b}, Targets: targets, Edges: edges, FinishedStates: map[string]model.WorkState{"a": model.WorkFailed}})
	if failed[0].State != model.WorkBlocked || failed[0].Reason != "dependency_failed" {
		t.Errorf("failed dep: %+v", failed[0])
	}
	if !Violates(order, edges, "b", "a") {
		t.Error("moving b above a must be refused")
	}
	if Violates(order, edges, "a", "c") {
		t.Error("moving a above c is fine")
	}

	worker := store.Worker{ID: "w1", Capabilities: map[string]string{"sandbox": "ready"}}
	repos := map[string]store.Repository{"equitizr": {Name: "equitizr", WorkerID: "w1"}, "other": {Name: "other", WorkerID: "w2"}}
	pick := Pick(PickInput{Order: order, Worker: worker, Repositories: repos, Concurrency: map[string]int{"r1": 1, "r2": 1}, Active: map[string]int{}})
	if pick.Target == nil || pick.Target.ID != "tc" {
		t.Fatalf("pick = %+v", pick)
	}
	// c is now running: interactive done; b blocked; a deferred; d on another worker.
	targets["c"][0].State = model.Running
	order = Order(QueueInput{Work: []store.Work{a, b, c, d}, Targets: targets, Edges: edges, Deferred: deferBacklog})
	pick = Pick(PickInput{Order: order, Worker: worker, Repositories: repos, Concurrency: map[string]int{"r1": 1}, Active: map[string]int{}})
	if pick.Target != nil {
		t.Errorf("nothing should be admissible, got %s", pick.Target.ID)
	}
	if pick.Skipped["td"] == "" {
		t.Errorf("d should be skipped for repository: %v", pick.Skipped)
	}
	// Budget clears: a is admissible unless the routine is at concurrency or a path lease intersects.
	order = Order(QueueInput{Work: []store.Work{a, b, c, d}, Targets: targets, Edges: edges})
	pick = Pick(PickInput{Order: order, Worker: worker, Repositories: repos, Concurrency: map[string]int{"r1": 1}, Active: map[string]int{"r1": 1}})
	if pick.Target != nil || pick.Skipped["ta"] == "" {
		t.Errorf("concurrency: %+v", pick)
	}
	pick = Pick(PickInput{Order: order, Worker: worker, Repositories: repos, Concurrency: map[string]int{"r1": 2}, Active: map[string]int{"r1": 1}, Leases: []PathLease{{TargetID: "tc", Repository: "equitizr"}}})
	if pick.Target != nil || pick.Skipped["ta"] != "path_lease held by tc" {
		t.Errorf("undeclared paths must serialise: %+v", pick)
	}
	a.Paths = []string{"docs/**"}
	order = Order(QueueInput{Work: []store.Work{a}, Targets: targets})
	pick = Pick(PickInput{Order: order, Worker: worker, Repositories: repos, Leases: []PathLease{{TargetID: "tc", Repository: "equitizr", Globs: []string{"internal/*"}}}})
	if pick.Target == nil {
		t.Errorf("disjoint globs must not conflict: %+v", pick)
	}
	pinned := store.Target{ID: "tp", WorkID: "a", Repository: "equitizr", State: model.Pending, WorkerID: "w9"}
	order = Order(QueueInput{Work: []store.Work{a}, Targets: map[string][]store.Target{"a": {pinned}}})
	pick = Pick(PickInput{Order: order, Worker: worker, Repositories: repos})
	if pick.Target != nil || pick.Skipped["tp"] != "pinned to worker w9" {
		t.Errorf("pinned: %+v", pick)
	}
	pick = Pick(PickInput{Order: Order(QueueInput{Work: []store.Work{a}, Targets: targets}), Worker: worker, Repositories: repos, Requirements: func(store.Work) []string { return []string{"browser"} }})
	if pick.Target != nil || pick.Skipped["ta"] != "worker lacks capability browser" {
		t.Errorf("capability: %+v", pick)
	}
}
