package model

import (
	"errors"
	"strings"
	"testing"
)

func TestIDsAndNames(t *testing.T) {
	a, b := NewID(), NewID()
	if a == b || len(a) != 32 || ValidateID(a) != nil {
		t.Fatalf("NewID = %q, %q", a, b)
	}
	if ShortID(a) != a[:8] || ShortID("abc") != "abc" {
		t.Error("ShortID")
	}
	for _, bad := range []string{"", "ABCDEF0123456789ABCDEF0123456789", a[:31], a + "0", "../" + a[3:]} {
		if ValidateID(bad) == nil {
			t.Errorf("ValidateID(%q) accepted", bad)
		}
	}
	for _, good := range []string{"a", "weekly-audit", "x9", strings.Repeat("a", 40)} {
		if err := ValidateName(good); err != nil {
			t.Errorf("ValidateName(%q): %v", good, err)
		}
	}
	for _, bad := range []string{"", "-a", "A", "a b", "a/b", "ad hoc", strings.Repeat("a", 41), "a_b"} {
		if ValidateName(bad) == nil {
			t.Errorf("ValidateName(%q) accepted", bad)
		}
	}
	if got := BranchName("inventory", "3f9a1c2e0000000000000000deadbeef"); got != "forge/inventory-3f9a1c2e" {
		t.Errorf("BranchName = %q", got)
	}
}

// TestTransitionTable lists every edge, allowed and refused, for both integrate
// values: the state machine is the product, so the table is exhaustive.
func TestTransitionTable(t *testing.T) {
	all := []State{Pending, Claimed, Preparing, Running, WaitingHuman, Verifying, Succeeded,
		Unverified, Failed, Cancelled, QueuedForMerge, Merging, Merged, Conflict}
	allowed := map[State][]State{
		Pending:        {Claimed, Cancelled},
		Claimed:        {Preparing, Failed, Cancelled},
		Preparing:      {Running, Failed, Cancelled},
		Running:        {WaitingHuman, Verifying, Failed, Cancelled},
		WaitingHuman:   {Pending, Cancelled},
		Verifying:      {Succeeded, Unverified, Cancelled},
		Succeeded:      {QueuedForMerge},
		QueuedForMerge: {Merging, Cancelled},
		Merging:        {Merged, Conflict, Unverified, QueuedForMerge, Cancelled},
		Conflict:       {QueuedForMerge, Cancelled},
	}
	for _, from := range all {
		for _, to := range all {
			for _, integrate := range []bool{false, true} {
				want := false
				for _, a := range allowed[from] {
					if a == to {
						want = true
					}
				}
				if from == Succeeded && !integrate {
					want = false
				}
				err := Transition(from, to, integrate)
				if (err == nil) != want {
					t.Errorf("Transition(%s→%s, integrate=%v) err=%v want allowed=%v", from, to, integrate, err, want)
				}
				if err != nil && !errors.Is(err, ErrTransition) {
					t.Errorf("error not wrapped: %v", err)
				}
			}
		}
	}
	if Transition("bogus", Claimed, false) == nil || Transition(Pending, "bogus", false) == nil {
		t.Error("unknown states accepted")
	}
	for _, s := range all {
		if !s.Valid() {
			t.Errorf("%s invalid", s)
		}
	}
}

func TestTerminalLeasedActive(t *testing.T) {
	cases := []struct {
		s              State
		integrate      bool
		terminal       bool
		leased, active bool
	}{
		{Succeeded, false, true, false, false},
		{Succeeded, true, false, false, false},
		{Merged, true, true, false, false},
		{Merged, false, true, false, false},
		{Failed, true, true, false, false},
		{Cancelled, false, true, false, false},
		{Unverified, false, true, false, false},
		{Running, false, false, true, true},
		{Claimed, false, false, true, true},
		{Preparing, false, false, true, true},
		{Verifying, false, false, false, false},
		{WaitingHuman, false, false, false, false},
		{Pending, false, false, false, false},
		{QueuedForMerge, true, false, false, false},
		{Merging, true, false, true, false},
		{Conflict, true, false, false, false},
	}
	for _, c := range cases {
		if IsTerminal(c.s, c.integrate) != c.terminal || Leased(c.s) != c.leased || Active(c.s) != c.active {
			t.Errorf("%s integrate=%v: terminal=%v leased=%v active=%v", c.s, c.integrate, IsTerminal(c.s, c.integrate), Leased(c.s), Active(c.s))
		}
	}
}

func TestDeriveWorkState(t *testing.T) {
	cases := []struct {
		name string
		in   WorkInputs
		want WorkState
	}{
		{"no targets", WorkInputs{}, WorkPending},
		{"all succeeded", WorkInputs{Targets: []State{Succeeded, Succeeded}}, WorkSucceeded},
		{"succeeded + unverified", WorkInputs{Targets: []State{Succeeded, Unverified}}, WorkUnverified},
		{"all cancelled", WorkInputs{Targets: []State{Cancelled}}, WorkCancelled},
		{"all failed", WorkInputs{Targets: []State{Failed, Failed}}, WorkFailed},
		{"mixed terminal", WorkInputs{Targets: []State{Succeeded, Failed}}, WorkPartial},
		{"cancelled + failed", WorkInputs{Targets: []State{Cancelled, Failed}}, WorkPartial},
		{"all merged", WorkInputs{Targets: []State{Merged}, Integrate: true}, WorkMerged},
		{"succeeded awaiting merge", WorkInputs{Targets: []State{Succeeded}, Integrate: true}, WorkMerging},
		{"succeeded and merged mid-flight", WorkInputs{Targets: []State{Succeeded, Merged}, Integrate: true}, WorkMerging},
		{"queued for merge", WorkInputs{Targets: []State{QueuedForMerge}, Integrate: true}, WorkMerging},
		{"conflict beats merging", WorkInputs{Targets: []State{Conflict, Merging}, Integrate: true}, WorkConflict},
		{"waiting human beats running", WorkInputs{Targets: []State{WaitingHuman, Running}}, WorkWaitingHuman},
		{"running with a terminal sibling", WorkInputs{Targets: []State{Succeeded, Running}}, WorkRunning},
		{"verifying is running", WorkInputs{Targets: []State{Verifying}}, WorkRunning},
		{"pending blocked", WorkInputs{Targets: []State{Pending}, Blocked: true, Deferred: true}, WorkBlocked},
		{"pending deferred", WorkInputs{Targets: []State{Pending}, Deferred: true}, WorkDeferred},
		{"pending", WorkInputs{Targets: []State{Pending, Succeeded}}, WorkPending},
		{"running beats blocked", WorkInputs{Targets: []State{Running, Pending}, Blocked: true}, WorkRunning},
	}
	for _, c := range cases {
		if got := DeriveWorkState(c.in); got != c.want {
			t.Errorf("%s: got %s want %s", c.name, got, c.want)
		}
	}
}

func TestAutonomyAndClasses(t *testing.T) {
	if got := ResolveAutonomy("", "", "", "", ""); got != AutonomyCheckpoint {
		t.Errorf("default = %s", got)
	}
	if got := ResolveAutonomy("", AutonomyAsk, AutonomyAuto, AutonomyAuto, AutonomyAuto); got != AutonomyAsk {
		t.Errorf("routine should beat repository/project: %s", got)
	}
	if got := ResolveAutonomy(AutonomyAuto, AutonomyAsk, "", "", ""); got != AutonomyAuto {
		t.Errorf("submit should win: %s", got)
	}
	if got := ResolveAutonomy("", "", AutonomyNotify, AutonomyAsk, ""); got != AutonomyNotify {
		t.Errorf("repository should beat project: %s", got)
	}
	if !AutonomyAsk.AllowsQuestions() || AutonomyAuto.AllowsQuestions() || Autonomy("loud").Valid() {
		t.Error("autonomy predicates")
	}
	if ClassInteractive.Rank() >= ClassNormal.Rank() || ClassNormal.Rank() >= ClassBacklog.Rank() || BudgetClass("x").Valid() {
		t.Error("class ranks")
	}
}

func TestWouldCycle(t *testing.T) {
	edges := []Edge{{Work: "b", BlockedBy: "a"}, {Work: "c", BlockedBy: "b"}}
	cases := []struct {
		cand Edge
		want bool
	}{
		{Edge{Work: "a", BlockedBy: "c"}, true},  // a→c→b→a
		{Edge{Work: "a", BlockedBy: "b"}, true},  // a→b→a
		{Edge{Work: "a", BlockedBy: "a"}, true},  // self
		{Edge{Work: "d", BlockedBy: "c"}, false}, // extends the chain
		{Edge{Work: "a", BlockedBy: "d"}, false}, // new root
		{Edge{Work: "c", BlockedBy: "a"}, false}, // redundant, not a cycle
	}
	for _, c := range cases {
		if got := WouldCycle(edges, c.cand); got != c.want {
			t.Errorf("WouldCycle(%v) = %v, want %v", c.cand, got, c.want)
		}
	}
}

func TestDependencies(t *testing.T) {
	edges := []Edge{{Work: "w", BlockedBy: "a", On: OnSuccess}, {Work: "w", BlockedBy: "b", On: OnTerminal}, {Work: "w", BlockedBy: "gone", On: OnSuccess}}
	st := Dependencies(edges, map[string]WorkState{"a": WorkRunning, "b": WorkFailed})
	if st.Satisfied || len(st.Waiting) != 1 || st.Waiting[0] != "a" || len(st.FailedDeps) != 1 || st.FailedDeps[0] != "gone" {
		t.Errorf("status = %+v", st)
	}
	st = Dependencies(edges[:2], map[string]WorkState{"a": WorkMerged, "b": WorkPending})
	if st.Satisfied || len(st.Waiting) != 1 || st.Waiting[0] != "b" {
		t.Errorf("status = %+v", st)
	}
	st = Dependencies(edges[:2], map[string]WorkState{"a": WorkSucceeded, "b": WorkCancelled})
	if !st.Satisfied {
		t.Errorf("status = %+v", st)
	}
	st = Dependencies(edges[:1], map[string]WorkState{"a": WorkPartial})
	if st.Satisfied || len(st.FailedDeps) != 1 {
		t.Errorf("partial success dep should be failed: %+v", st)
	}
	stacked := []Edge{{Work: "w", BlockedBy: "a", On: OnSuccess, StackOn: true}}
	if !Dependencies(stacked, map[string]WorkState{"a": WorkMerging}).Satisfied {
		t.Error("stack_on should be satisfied while the dependency awaits merge")
	}
	if Dependencies(edges[:1], map[string]WorkState{"a": WorkMerging}).Satisfied {
		t.Error("a plain on:success dependency must wait for merged")
	}
	for _, s := range []State{Succeeded, QueuedForMerge, Merging, Merged} {
		if !IsSuccess(s) {
			t.Errorf("IsSuccess(%s) false", s)
		}
	}
	for _, s := range []State{Unverified, Failed, Cancelled, Conflict, Running} {
		if IsSuccess(s) {
			t.Errorf("IsSuccess(%s) true", s)
		}
	}
}
