// Package flow is the workflow run engine's pure half: given a frozen graph,
// the run's node instances, and the derived states of the Works routine
// instances materialized, Evaluate computes every transition the run owes —
// instances to create, statuses to advance, Works to start, journal events —
// as data. It has no I/O; the driver (internal/web) applies the diff in one
// store transaction with compare-and-set guards, so applying is idempotent
// and a daemon restart just re-evaluates.
//
// The execution model is token passing. When an instance reaches a terminal
// status, each of its outgoing edges is decided — taken or dead — exactly
// once, in the same transaction. A decision lands in the target instance's
// Edges map (creating the instance if the token is its first). An instance is
// ready when it has ≥1 taken edge and every incoming non-loop edge is decided
// (join(any): the first taken edge suffices; iteration > 1: a fresh token
// alone suffices, because a loop's later waves are driven only by re-fired
// edges). An instance whose every incoming edge went dead is skipped, and its
// own edges all go dead — the cascade that replaces the queue's permanent
// dependency_failed wedge. A loop edge taken creates the next iteration of
// its target, immediately ready, until the edge's cap; at the cap it goes
// dead and is journaled.
package flow

import (
	"encoding/json"
	"fmt"
	"sort"
	"strconv"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

// Input is everything Evaluate looks at.
type Input struct {
	RunID string
	Graph *store.WorkflowGraph
	// RunStatus is the stored status; the diff proposes a change only when
	// the evaluation lands somewhere else.
	RunStatus string
	// Nodes is every instance row of the run, any order.
	Nodes []store.RunNode
	// WorkStates maps a routine instance's Work id to its derived state.
	WorkStates map[string]model.WorkState
	// WorkOutputs optionally maps a terminal Work to the enriched node output
	// the driver assembled from its result envelope ({state, summary, output,
	// targets}); absent, the node output is just {"state": ...}.
	WorkOutputs map[string]json.RawMessage
	// ScriptResults carries the driver's finished script/switch executions,
	// keyed by instance id. Execution happens outside the write transaction;
	// the result finalizes the instance and delivers its tokens here, so the
	// finalization and the routing land atomically.
	ScriptResults map[string]ScriptResult
	// MaterializeFailures carries definitional Work-creation failures (an
	// unregistered repository, an archived routine) for ready routine
	// instances, keyed by instance id: the instance fails with the message
	// and its tokens deliver like any failure, instead of the run wedging on
	// a transaction that can never commit.
	MaterializeFailures map[string]string
}

// ScriptResult is one script or switch execution's outcome.
type ScriptResult struct {
	Status string // NodeSucceeded | NodeFailed
	Output json.RawMessage
	Error  string
}

// Update is one instance advancing, anchored to the status it leaves so the
// driver's write is a compare-and-set.
type Update struct {
	Node store.RunNode
	From string
}

// Start is a ready routine instance the driver must materialize into a Work.
// BlockedBy carries already-satisfied edges to the upstream Works (stack_on
// rides them; provenance and lineage views keep working).
type Start struct {
	InstanceID string
	NodeID     string
	Iteration  int
	Config     store.RoutineNodeConfig
	BlockedBy  []model.Edge
}

// Event is a journal row the driver writes with the diff.
type Event struct {
	Kind    string
	Payload map[string]any
}

// Diff is what one evaluation owes the database. Empty() means the run is
// stable until something external (a Work, a kick) changes.
type Diff struct {
	Creates   []store.RunNode
	Updates   []Update
	Starts    []Start
	Events    []Event
	RunStatus string // "" = still running / unchanged
}

// Empty reports whether applying the diff would change nothing.
func (d Diff) Empty() bool {
	return len(d.Creates) == 0 && len(d.Updates) == 0 && len(d.Starts) == 0 && d.RunStatus == ""
}

// EdgeKey names an incoming edge in an instance's Edges map. Validation
// guarantees (from, when, case) is unique per target.
func EdgeKey(e store.WorkflowGraphEdge) string {
	k := e.From + "|" + string(e.When)
	if e.Default {
		return k + "|default"
	}
	if e.Case != "" {
		return k + "|" + e.Case
	}
	return k
}

const (
	decisionTaken = "taken"
	decisionDead  = "dead"
)

// NodeStatusForWork maps a Work's derived state to the routine instance's
// terminal status; ok is false while the Work is still in flight
// (waiting_human, merging, and conflict stay running — the attention
// machinery owns them, the run just is not done yet).
func NodeStatusForWork(st model.WorkState) (string, bool) {
	switch st {
	case model.WorkSucceeded, model.WorkMerged:
		return store.NodeSucceeded, true
	case model.WorkFailed, model.WorkUnverified, model.WorkPartial:
		return store.NodeFailed, true
	case model.WorkCancelled:
		return store.NodeCancelled, true
	}
	return "", false
}

// evaluator is one Evaluate call's working state: instances by node id, each
// list sorted by iteration, with in-memory copies the fixpoint mutates.
type evaluator struct {
	current  string // the run's stored status
	graph    *store.WorkflowGraph
	incoming map[string][]store.WorkflowGraphEdge // non-loop, by target
	outgoing map[string][]store.WorkflowGraphEdge // all, by source
	byNode   map[string][]*store.RunNode
	total    int
	diff     *Diff
	// touched tracks instances whose row changed (created or updated) so the
	// diff carries each once.
	created map[*store.RunNode]bool
	updated map[*store.RunNode]string // instance → status it left in the DB
}

// Evaluate computes the run's next transitions. It never mutates Input.
func Evaluate(in Input) (Diff, error) {
	ev := &evaluator{
		current:  in.RunStatus,
		graph:    in.Graph,
		incoming: map[string][]store.WorkflowGraphEdge{},
		outgoing: map[string][]store.WorkflowGraphEdge{},
		byNode:   map[string][]*store.RunNode{},
		diff:     &Diff{},
		created:  map[*store.RunNode]bool{},
		updated:  map[*store.RunNode]string{},
	}
	for _, e := range in.Graph.Edges {
		ev.outgoing[e.From] = append(ev.outgoing[e.From], e)
		if !e.Loop {
			ev.incoming[e.To] = append(ev.incoming[e.To], e)
		}
	}
	for i := range in.Nodes {
		n := in.Nodes[i] // copy; the diff owns its mutations
		if n.Edges == nil {
			n.Edges = map[string]string{}
		}
		ev.byNode[n.NodeID] = append(ev.byNode[n.NodeID], &n)
		ev.total++
	}
	for _, list := range ev.byNode {
		sort.Slice(list, func(i, j int) bool { return list[i].Iteration < list[j].Iteration })
	}

	// Pass 1: routine instances whose Work went terminal, and script/switch
	// instances the driver just executed, finalize now; their tokens deliver
	// in the fixpoint below.
	var frontier []*store.RunNode
	for _, list := range ev.byNode {
		for _, inst := range list {
			switch {
			case inst.Status == store.NodeRunning && inst.Type == store.NodeRoutine:
				st, ok := in.WorkStates[inst.WorkID]
				status, terminal := NodeStatusForWork(st)
				if !ok || !terminal {
					continue
				}
				out := `{"state":` + strconv.Quote(string(st)) + `}`
				if enriched, ok := in.WorkOutputs[inst.WorkID]; ok && len(enriched) > 0 {
					out = string(enriched)
				}
				ev.finalize(inst, status, out, "")
				frontier = append(frontier, inst)
			case inst.Status == store.NodeReady && (inst.Type == store.NodeScript || inst.Type == store.NodeSwitch):
				res, ok := in.ScriptResults[inst.ID]
				if !ok {
					continue
				}
				ev.finalize(inst, res.Status, string(res.Output), res.Error)
				frontier = append(frontier, inst)
			case inst.Status == store.NodeReady && inst.Type == store.NodeRoutine:
				msg, ok := in.MaterializeFailures[inst.ID]
				if !ok {
					continue
				}
				ev.finalize(inst, store.NodeFailed, "", msg)
				frontier = append(frontier, inst)
			}
		}
	}
	// Seed: a run with no instances yet gets its roots.
	if ev.total == 0 {
		for _, n := range in.Graph.Nodes {
			if len(ev.incoming[n.ID]) == 0 {
				ev.create(&store.RunNode{RunID: in.RunID, NodeID: n.ID, Iteration: 1, Type: n.Type, Status: store.NodePending, Edges: map[string]string{}})
			}
		}
	}

	// Fixpoint: deliver tokens, promote readiness, complete bodiless nodes.
	for i := 0; ; i++ {
		if i > 10*store.MaxRunInstances {
			return Diff{}, fmt.Errorf("run %s: evaluation did not settle", in.RunID)
		}
		progress := false
		for _, inst := range frontier {
			ev.deliver(inst)
			progress = true
		}
		frontier = nil
		// Promote pending instances whose conditions are met, and complete
		// nodes with no body (joins) immediately.
		for _, list := range ev.byNode {
			for _, inst := range list {
				if inst.Status != store.NodePending {
					continue
				}
				switch ev.readiness(inst) {
				case store.NodeReady:
					ev.setStatus(inst, store.NodeReady)
					progress = true
				case store.NodeSkipped:
					ev.finalize(inst, store.NodeSkipped, "", "no incoming edge was taken")
					ev.diff.Events = append(ev.diff.Events, Event{Kind: "workflow.node_skipped", Payload: map[string]any{"run": in.RunID, "node": inst.NodeID, "iteration": inst.Iteration}})
					frontier = append(frontier, inst)
					progress = true
				}
			}
		}
		for _, list := range ev.byNode {
			for _, inst := range list {
				if inst.Status != store.NodeReady {
					continue
				}
				switch inst.Type {
				case store.NodeJoin:
					ev.finalize(inst, store.NodeSucceeded, "", "")
					frontier = append(frontier, inst)
					progress = true
				case store.NodeRoutine, store.NodeScript, store.NodeSwitch:
					// Routine: the driver materializes. Script/switch: the
					// driver executes. Both stay ready here.
				}
			}
		}
		if !progress && len(frontier) == 0 {
			break
		}
	}
	if ev.total > store.MaxRunInstances {
		if ev.current != store.RunFailed {
			ev.diff.RunStatus = store.RunFailed
			ev.diff.Events = append(ev.diff.Events, Event{Kind: "workflow.run_overrun", Payload: map[string]any{"run": in.RunID, "instances": ev.total, "max": store.MaxRunInstances}})
		}
		ev.emit()
		return *ev.diff, nil
	}

	// Ready routine instances become Starts.
	for _, list := range ev.byNode {
		for _, inst := range list {
			if inst.Status != store.NodeReady || inst.Type != store.NodeRoutine {
				continue
			}
			def := ev.graph.Node(inst.NodeID)
			cfg, err := def.RoutineConfig()
			if err != nil {
				return Diff{}, err
			}
			ev.diff.Starts = append(ev.diff.Starts, Start{
				InstanceID: inst.ID, NodeID: inst.NodeID, Iteration: inst.Iteration,
				Config: cfg, BlockedBy: ev.satisfiedEdges(inst),
			})
		}
	}
	sort.Slice(ev.diff.Starts, func(i, j int) bool { return ev.diff.Starts[i].NodeID < ev.diff.Starts[j].NodeID })

	ev.runStatus()
	ev.emit()
	return *ev.diff, nil
}

// create registers a new in-memory instance.
func (ev *evaluator) create(n *store.RunNode) {
	ev.byNode[n.NodeID] = append(ev.byNode[n.NodeID], n)
	ev.created[n] = true
	ev.total++
}

// setStatus advances an instance, remembering the DB status for the CAS.
func (ev *evaluator) setStatus(inst *store.RunNode, status string) {
	if !ev.created[inst] {
		if _, tracked := ev.updated[inst]; !tracked {
			ev.updated[inst] = inst.Status
		}
	}
	inst.Status = status
}

// finalize moves an instance to a terminal status with output and error.
func (ev *evaluator) finalize(inst *store.RunNode, status, output, errMsg string) {
	ev.setStatus(inst, status)
	if output != "" {
		inst.Output = json.RawMessage(output)
	}
	if errMsg != "" {
		inst.Error = errMsg
	}
}

// latest is the newest instance of a node, or nil.
func (ev *evaluator) latest(nodeID string) *store.RunNode {
	list := ev.byNode[nodeID]
	if len(list) == 0 {
		return nil
	}
	return list[len(list)-1]
}

// decision is the outcome of edge e given its source instance's terminal
// status.
func (ev *evaluator) decision(e store.WorkflowGraphEdge, inst *store.RunNode) string {
	switch inst.Status {
	case store.NodeSucceeded:
		switch {
		case e.When == store.WhenCase || e.Default:
			return ev.switchDecision(e, inst)
		case e.When == "" || e.When == store.WhenSuccess || e.When == store.WhenAlways:
			return decisionTaken
		}
		return decisionDead
	case store.NodeFailed:
		if e.When == store.WhenFailure || e.When == store.WhenAlways {
			return decisionTaken
		}
		return decisionDead
	case store.NodeSkipped, store.NodeCancelled:
		return decisionDead
	}
	return ""
}

// switchDecision matches a case edge against the switch's chosen value,
// carried in the instance output as {"case": "<value>"} by the switch
// executor. The default edge takes when no sibling case edge matches.
func (ev *evaluator) switchDecision(e store.WorkflowGraphEdge, inst *store.RunNode) string {
	var out struct {
		Case string `json:"case"`
	}
	if len(inst.Output) > 0 {
		if err := json.Unmarshal(inst.Output, &out); err != nil {
			return decisionDead
		}
	}
	if e.Default {
		for _, sib := range ev.outgoing[inst.NodeID] {
			if sib.When == store.WhenCase && sib.Case == out.Case {
				return decisionDead
			}
		}
		return decisionTaken
	}
	if e.Case == out.Case {
		return decisionTaken
	}
	return decisionDead
}

// deliver decides every outgoing edge of a just-terminal instance and routes
// the tokens.
func (ev *evaluator) deliver(inst *store.RunNode) {
	for _, e := range ev.outgoing[inst.NodeID] {
		d := ev.decision(e, inst)
		if d == "" {
			continue
		}
		if e.Loop {
			ev.deliverLoop(e, d)
			continue
		}
		ev.deliverToken(e, d, inst.Iteration)
	}
}

// deliverLoop fires a loop edge: the next iteration of its target, ready
// immediately, until the cap.
func (ev *evaluator) deliverLoop(e store.WorkflowGraphEdge, d string) {
	if d != decisionTaken {
		return
	}
	count := len(ev.byNode[e.To])
	if count >= 1+e.MaxIterations {
		ev.diff.Events = append(ev.diff.Events, Event{Kind: "workflow.loop_capped", Payload: map[string]any{"edge": e.From + "→" + e.To, "max_iterations": e.MaxIterations}})
		return
	}
	def := ev.graph.Node(e.To)
	next := &store.RunNode{NodeID: e.To, Iteration: count + 1, Type: def.Type, Status: store.NodeReady, Edges: map[string]string{EdgeKey(e): decisionTaken}}
	ev.create(next)
	ev.diff.Events = append(ev.diff.Events, Event{Kind: "workflow.loop_iteration", Payload: map[string]any{"node": e.To, "iteration": next.Iteration, "max_iterations": e.MaxIterations}})
}

// deliverToken lands a non-loop decision on the target's current instance,
// creating the instance (or, for a token from a newer wave arriving after the
// target finished, the next iteration) as needed. srcIteration is the source
// instance's iteration: a token from the same wave as an already-started
// target is a straggler (a join(any) loser) and is dropped, while a token
// from a later wave (a loop re-entry upstream) re-fires the node.
func (ev *evaluator) deliverToken(e store.WorkflowGraphEdge, d string, srcIteration int) {
	key := EdgeKey(e)
	inst := ev.latest(e.To)
	switch {
	case inst == nil:
		def := ev.graph.Node(e.To)
		ev.create(&store.RunNode{NodeID: e.To, Iteration: 1, Type: def.Type, Status: store.NodePending, Edges: map[string]string{key: d}})
	case inst.Status == store.NodePending:
		if _, dup := inst.Edges[key]; !dup {
			ev.setEdge(inst, key, d)
		}
	default:
		if d != decisionTaken || srcIteration <= inst.Iteration {
			return
		}
		def := ev.graph.Node(e.To)
		ev.create(&store.RunNode{NodeID: e.To, Iteration: inst.Iteration + 1, Type: def.Type, Status: store.NodePending, Edges: map[string]string{key: d}})
	}
}

// setEdge records a decision on an instance's Edges map (a row change).
func (ev *evaluator) setEdge(inst *store.RunNode, key, d string) {
	if !ev.created[inst] {
		if _, tracked := ev.updated[inst]; !tracked {
			ev.updated[inst] = inst.Status
		}
	}
	inst.Edges[key] = d
}

// readiness classifies a pending instance: ready, skipped, or "" (waiting).
func (ev *evaluator) readiness(inst *store.RunNode) string {
	taken, decided := 0, 0
	for _, d := range inst.Edges {
		decided++
		if d == decisionTaken {
			taken++
		}
	}
	expected := len(ev.incoming[inst.NodeID])
	if expected == 0 {
		// A root: exists only because the seed created it; ready by birthright.
		return store.NodeReady
	}
	anyMode := false
	if def := ev.graph.Node(inst.NodeID); def.Type == store.NodeJoin {
		if cfg, err := def.JoinConfig(); err == nil && cfg.Mode == "any" {
			anyMode = true
		}
	}
	switch {
	case taken > 0 && (inst.Iteration > 1 || anyMode || decided >= expected):
		return store.NodeReady
	case taken == 0 && decided >= expected:
		return store.NodeSkipped
	}
	return ""
}

// satisfiedEdges are the already-satisfied dependency edges a materialized
// Work carries to its upstream Works: stack_on rides them and lineage views
// keep working.
func (ev *evaluator) satisfiedEdges(inst *store.RunNode) []model.Edge {
	var out []model.Edge
	for _, e := range ev.incoming[inst.NodeID] {
		if inst.Edges[EdgeKey(e)] != decisionTaken {
			continue
		}
		src := ev.sourceWithWork(e.From)
		if src == nil {
			continue
		}
		on := model.OnSuccess
		if e.When == store.WhenAlways || e.When == store.WhenFailure {
			on = model.OnTerminal
		}
		out = append(out, model.Edge{BlockedBy: src.WorkID, On: on, StackOn: e.StackOn})
	}
	return out
}

// sourceWithWork is the newest instance of a node that has a Work.
func (ev *evaluator) sourceWithWork(nodeID string) *store.RunNode {
	list := ev.byNode[nodeID]
	for i := len(list) - 1; i >= 0; i-- {
		if list[i].WorkID != "" {
			return list[i]
		}
	}
	return nil
}

// runStatus decides whether the run is done. Any live instance keeps it
// running; otherwise the latest instance of each node decides: a failure
// anywhere fails the run, a cancellation cancels it, and skips are routing,
// not errors.
func (ev *evaluator) runStatus() {
	failed, cancelled := false, false
	for _, list := range ev.byNode {
		for _, inst := range list {
			if !store.NodeTerminal(inst.Status) {
				return // still running
			}
		}
		switch list[len(list)-1].Status {
		case store.NodeFailed:
			failed = true
		case store.NodeCancelled:
			cancelled = true
		}
	}
	if len(ev.byNode) == 0 {
		return
	}
	status := store.RunSucceeded
	switch {
	case failed:
		status = store.RunFailed
	case cancelled:
		status = store.RunCancelled
	}
	if status != ev.current {
		ev.diff.RunStatus = status
	}
}

// emit converts the tracked mutations into the diff's Creates and Updates.
func (ev *evaluator) emit() {
	for _, list := range ev.byNode {
		for _, inst := range list {
			if ev.created[inst] {
				ev.diff.Creates = append(ev.diff.Creates, *inst)
			} else if from, ok := ev.updated[inst]; ok {
				ev.diff.Updates = append(ev.diff.Updates, Update{Node: *inst, From: from})
			}
		}
	}
	sort.Slice(ev.diff.Creates, func(i, j int) bool {
		a, b := ev.diff.Creates[i], ev.diff.Creates[j]
		if a.NodeID != b.NodeID {
			return a.NodeID < b.NodeID
		}
		return a.Iteration < b.Iteration
	})
	sort.Slice(ev.diff.Updates, func(i, j int) bool { return ev.diff.Updates[i].Node.ID < ev.diff.Updates[j].Node.ID })
}
