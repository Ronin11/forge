package model

import "fmt"

// State is a Target's position in its lifecycle. The merge states exist from the
// first migration so the transition table never grows a second home; until a
// Work sets integrate = true, succeeded is simply terminal.
type State string

const (
	Pending        State = "pending"
	Claimed        State = "claimed"
	Preparing      State = "preparing"
	Running        State = "running"
	WaitingHuman   State = "waiting_human"
	Verifying      State = "verifying"
	Succeeded      State = "succeeded"
	Unverified     State = "unverified"
	Failed         State = "failed"
	Cancelled      State = "cancelled"
	QueuedForMerge State = "queued_for_merge"
	Merging        State = "merging"
	Merged         State = "merged"
	Conflict       State = "conflict"
)

// transitions is the whole rule. Every allowed edge is listed; nothing else is.
var transitions = map[State][]State{
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
	// M11 retry (forge task retry, DESIGN.md §22): a terminal, non-merged
	// Target may go back to pending for a fresh attempt. Exactly these three;
	// merged stays terminal, and succeeded is already the accepted outcome.
	// unverified → verifying is `forge task reverify`: the completed work is
	// re-checked by a fresh verify attempt without re-running the subject —
	// the recovery when the verifier, not the subject, is what failed.
	Failed:     {Pending},
	Unverified: {Pending, Verifying},
	Cancelled:  {Pending},
}

// The merging edges: merged (pushed); conflict (rebase failed and integrate mode
// could not fix it); unverified (checks failed on the rebased result); back to
// queued_for_merge (the integrator's lease expired — the sweeper retries, capped);
// cancelled (a human gave up).

// ErrTransition is wrapped by every refused transition.
var ErrTransition = fmt.Errorf("transition not allowed")

// Transition is the only function that moves a Target between states. integrate
// is the Work's flag: without it succeeded is terminal and the merge edge is
// refused.
func Transition(from, to State, integrate bool) error {
	if !from.Valid() || !to.Valid() {
		return fmt.Errorf("%w: %q → %q: unknown state", ErrTransition, from, to)
	}
	if from == Succeeded && !integrate {
		return fmt.Errorf("%w: %q → %q: succeeded is terminal without integration", ErrTransition, from, to)
	}
	for _, allowed := range transitions[from] {
		if allowed == to {
			return nil
		}
	}
	return fmt.Errorf("%w: %q → %q", ErrTransition, from, to)
}

// Valid reports whether s is a known state.
func (s State) Valid() bool {
	switch s {
	case Pending, Claimed, Preparing, Running, WaitingHuman, Verifying, Succeeded,
		Unverified, Failed, Cancelled, QueuedForMerge, Merging, Merged, Conflict:
		return true
	}
	return false
}

// IsTerminal is the one place that knows succeeded is terminal only when the Work
// does not integrate.
func IsTerminal(s State, integrate bool) bool {
	switch s {
	case Unverified, Failed, Cancelled, Merged:
		return true
	case Succeeded:
		return !integrate
	}
	return false
}

// Leased reports whether a Target in s holds a worker lease (and so is subject to
// the sweeper). The integrator runs on a worker under a merge claim, so merging is
// leased too.
func Leased(s State) bool {
	return s == Claimed || s == Preparing || s == Running || s == Merging
}

// Active reports whether a Target in s counts against its routine's concurrency.
// Merging does not: the integrator is serial per repository already.
func Active(s State) bool {
	return s == Claimed || s == Preparing || s == Running
}

// HoldsWriteSet reports whether a Target in s holds its Work's path lease
// (DESIGN.md §10.2, §20): the write set is leased from claim until the agent
// work is decided. waiting_human and the merge states hold no lease — a
// resume re-acquires it at claim, and the merge queue serialises per
// repository on its own.
func HoldsWriteSet(s State) bool {
	return s == Claimed || s == Preparing || s == Running || s == Verifying
}

// IsSuccess is the one definition of "the agent's work was accepted": stats'
// verified success, on:success dependencies, and the derived Work state all use
// it. For integrating Work the merge states are successes in flight.
func IsSuccess(s State) bool {
	return s == Succeeded || s == QueuedForMerge || s == Merging || s == Merged
}

// WorkState is derived from a Work's Targets and never stored.
type WorkState string

const (
	WorkPending      WorkState = "pending"
	WorkBlocked      WorkState = "blocked"
	WorkDeferred     WorkState = "deferred"
	WorkRunning      WorkState = "running"
	WorkWaitingHuman WorkState = "waiting_human"
	WorkMerging      WorkState = "merging"
	WorkSucceeded    WorkState = "succeeded"
	WorkUnverified   WorkState = "unverified"
	WorkFailed       WorkState = "failed"
	WorkCancelled    WorkState = "cancelled"
	WorkPartial      WorkState = "partial"
	WorkMerged       WorkState = "merged"
	WorkConflict     WorkState = "conflict"
)

// WorkInputs is what DeriveWorkState looks at. blocked and deferred are computed
// by the scheduler from dependencies and the budget policy respectively.
type WorkInputs struct {
	Targets   []State
	Integrate bool
	Blocked   bool
	Deferred  bool
}

// DeriveWorkState is the one home for a Work's state. Precedence: all terminal →
// the terminal outcome; else waiting_human; else conflict (a human must act);
// else running; else merging (an integrating Work whose Targets succeeded and are
// awaiting, or in, the merge queue); else blocked; else deferred; else pending.
func DeriveWorkState(in WorkInputs) WorkState {
	if len(in.Targets) == 0 {
		return WorkPending
	}
	allTerminal := true
	var succeeded, unverified, failed, cancelled, merged int
	for _, s := range in.Targets {
		if !IsTerminal(s, in.Integrate) {
			allTerminal = false
			continue
		}
		switch s {
		case Succeeded:
			succeeded++
		case Unverified:
			unverified++
		case Failed:
			failed++
		case Cancelled:
			cancelled++
		case Merged:
			merged++
		}
	}
	n := len(in.Targets)
	if allTerminal {
		switch {
		case merged == n:
			return WorkMerged
		case succeeded == n:
			return WorkSucceeded
		case succeeded+unverified == n:
			return WorkUnverified
		case cancelled == n:
			return WorkCancelled
		case failed == n:
			return WorkFailed
		}
		return WorkPartial
	}
	if has(in.Targets, WaitingHuman) {
		return WorkWaitingHuman
	}
	if has(in.Targets, Conflict) {
		return WorkConflict
	}
	if has(in.Targets, Claimed, Preparing, Running, Verifying) {
		return WorkRunning
	}
	if in.Integrate && has(in.Targets, Succeeded, QueuedForMerge, Merging) {
		return WorkMerging
	}
	if in.Blocked {
		return WorkBlocked
	}
	if in.Deferred {
		return WorkDeferred
	}
	return WorkPending
}

func has(states []State, any ...State) bool {
	for _, s := range states {
		for _, a := range any {
			if s == a {
				return true
			}
		}
	}
	return false
}

// FailureReason is the enum-like string on a failed Target; one home, so stats
// never see two spellings.
type FailureReason string

const (
	ReasonExitNonzero        FailureReason = "exit_nonzero"
	ReasonTimeout            FailureReason = "timeout"
	ReasonCancelled          FailureReason = "cancelled"
	ReasonLeaseExpired       FailureReason = "lease_expired"
	ReasonWorkerRestart      FailureReason = "worker_restart"
	ReasonLaunchFailed       FailureReason = "launch_failed"
	ReasonPrepareFailed      FailureReason = "prepare_failed"
	ReasonAmbiguityAtAuto    FailureReason = "ambiguity_at_auto"
	ReasonResultUnparseable  FailureReason = "result_unparseable"
	ReasonBudgetExceeded     FailureReason = "budget_exceeded"
	ReasonWorktreeLost       FailureReason = "worktree_lost"
	ReasonAskBudgetExhausted FailureReason = "ask_budget_exhausted"
	ReasonInternal           FailureReason = "internal"
)

// Trigger is how a Work came to exist.
type Trigger string

const (
	TriggerManual     Trigger = "manual"
	TriggerSchedule   Trigger = "schedule"
	TriggerProposal   Trigger = "proposal"
	TriggerDependency Trigger = "dependency"
	TriggerPlugin     Trigger = "plugin"
)

// Cause is the functional reason a Work was spawned — the machine label beside
// caused_by_work_id (DESIGN.md §3 "Provenance"). Empty for a root (a manual
// submission, a routine firing, a plan).
type Cause string

const (
	CausePlanTask Cause = "plan_task"
	CauseVerify   Cause = "verify"
	CauseFollowUp Cause = "follow_up"
)

// Valid reports whether c is empty (a root) or one of the known causes.
func (c Cause) Valid() bool {
	switch c {
	case "", CausePlanTask, CauseVerify, CauseFollowUp:
		return true
	}
	return false
}

// BudgetClass orders Work in the queue and gates admission under the budget policy.
type BudgetClass string

const (
	ClassInteractive BudgetClass = "interactive"
	ClassNormal      BudgetClass = "normal"
	ClassBacklog     BudgetClass = "backlog"
)

// Rank orders classes for the queue: interactive first.
func (c BudgetClass) Rank() int {
	switch c {
	case ClassInteractive:
		return 0
	case ClassNormal:
		return 1
	case ClassBacklog:
		return 2
	}
	return 3
}

// Valid reports whether c is a known class.
func (c BudgetClass) Valid() bool { return c.Rank() < 3 }

// Autonomy is the slider: how much an agent may do before a human is consulted.
type Autonomy string

const (
	AutonomyAsk        Autonomy = "ask"
	AutonomyCheckpoint Autonomy = "checkpoint"
	AutonomyNotify     Autonomy = "notify"
	AutonomyAuto       Autonomy = "auto"
)

// Valid reports whether a is a known level.
func (a Autonomy) Valid() bool {
	switch a {
	case AutonomyAsk, AutonomyCheckpoint, AutonomyNotify, AutonomyAuto:
		return true
	}
	return false
}

// AllowsQuestions reports whether an attempt at this level may pause for a human.
func (a Autonomy) AllowsQuestions() bool { return a == AutonomyAsk || a == AutonomyCheckpoint }

// Criticality is how urgently a Question needs a human. Only critical always
// blocks for a person; normal and low may be auto-decided once their
// time-of-day SLA lapses (the attention sweep, DESIGN.md §10.4). Agent-declared,
// default normal.
type Criticality string

const (
	CriticalityCritical Criticality = "critical"
	CriticalityNormal   Criticality = "normal"
	CriticalityLow      Criticality = "low"
)

// Valid reports whether c is a known criticality.
func (c Criticality) Valid() bool {
	switch c {
	case CriticalityCritical, CriticalityNormal, CriticalityLow:
		return true
	}
	return false
}

// AutoDecidable reports whether a Question at this criticality may be auto-decided
// after its wait lapses; critical never is.
func (c Criticality) AutoDecidable() bool { return c == CriticalityNormal || c == CriticalityLow }

// ResolveAutonomy is the precedence chain: Work submit override > routine >
// repository forge.toml > project > mode default. Empty means "not set".
func ResolveAutonomy(submit, routine, repository, project, mode Autonomy) Autonomy {
	for _, a := range []Autonomy{submit, routine, repository, project, mode} {
		if a != "" {
			return a
		}
	}
	return AutonomyCheckpoint
}

// VerificationLevel is how far a claim was checked (VERIFICATION.md): L0
// consistency, L1 declared checks, L2 behavioural, L3 human sign-off.
type VerificationLevel int

// Levels. L0 is the minimum every mode gets.
const (
	L0 VerificationLevel = iota
	L1
	L2
	L3
)

// Valid reports whether l is a defined level.
func (l VerificationLevel) Valid() bool { return l >= L0 && l <= L3 }

// WriteScope is what a mode may change; L0 enforces it against git
// (VERIFICATION.md).
type WriteScope string

// Scopes. NewProject is greenfield's fresh directory.
const (
	WritesNone       WriteScope = "none"
	WritesKbOnly     WriteScope = "kb_only"
	WritesDocsOnly   WriteScope = "docs_only"
	WritesRepo       WriteScope = "repo"
	WritesNewProject WriteScope = "new_project"
)

// Valid reports whether w is a defined scope.
func (w WriteScope) Valid() bool {
	switch w {
	case WritesNone, WritesKbOnly, WritesDocsOnly, WritesRepo, WritesNewProject:
		return true
	}
	return false
}
