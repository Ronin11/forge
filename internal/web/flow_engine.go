package web

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"time"

	"forge/internal/core/engine"
	"forge/internal/core/flow"
	"forge/internal/core/model"
	"forge/internal/core/store"
)

// The workflow run engine's driver. flow.Evaluate is the pure half — this
// file reads a run's rows off the pool, executes ready scripts OUTSIDE any
// write transaction (JavaScript must never run inside SQLite's single
// writer), and applies the resulting diff in one compare-and-set-guarded
// transaction. Applying is idempotent, so the loop's 15s scan of open runs is
// the restart story and the latency kicks (run creation, work completion,
// cancellation) are only that — latency.

// KickFlow advances a run synchronously, after the caller's transaction has
// committed. Advancing is cheap (a few pool reads, one CAS-guarded write when
// something changed) and per-run serialized, so completion and cancellation
// handlers call it inline — which also makes runs deterministic under test.
func (s *Server) KickFlow(ctx context.Context, runID string) {
	if runID == "" {
		return
	}
	s.advanceRun(ctx, runID)
}

// RunFlowEngine is the daemon loop: every interval, advance every open run.
// The kicks give latency; this loop gives restart recovery and catches
// transitions with no HTTP hook (integrator merges, sweeper lease expiries).
func (s *Server) RunFlowEngine(ctx context.Context, interval time.Duration) {
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			ids, err := s.store.OpenWorkflowRuns(ctx)
			if err != nil {
				s.log.ErrorContext(ctx, "list open workflow runs", "error", err)
				continue
			}
			for _, id := range ids {
				s.advanceRun(ctx, id)
			}
		}
	}
}

// flowRunLock serializes advances of one run in-process; correctness rests on
// the CAS writes, this only avoids duplicate script executions.
func (s *Server) flowRunLock(runID string) *sync.Mutex {
	s.flowMu.Lock()
	defer s.flowMu.Unlock()
	if s.flowLocks == nil {
		s.flowLocks = map[string]*sync.Mutex{}
	}
	mu, ok := s.flowLocks[runID]
	if !ok {
		mu = &sync.Mutex{}
		s.flowLocks[runID] = mu
	}
	return mu
}

// advanceRun evaluates and applies one run until it is stable. Errors are
// logged — the next tick retries; nothing else handles them.
func (s *Server) advanceRun(ctx context.Context, runID string) {
	mu := s.flowRunLock(runID)
	mu.Lock()
	defer mu.Unlock()
	for range 20 {
		again, err := s.advanceOnce(ctx, runID)
		if errors.Is(err, store.ErrStaleGeneration) || errors.Is(err, store.ErrConflict) {
			continue // lost a race; re-read and re-evaluate
		}
		if err != nil {
			s.log.ErrorContext(ctx, "advance workflow run", "run_id", runID, "error", err)
			return
		}
		if !again {
			return
		}
	}
	s.log.WarnContext(ctx, "workflow run did not settle in one advance", "run_id", runID)
}

// advanceOnce is one read-evaluate-execute-apply cycle. It reports whether
// anything changed (the caller loops while it does).
func (s *Server) advanceOnce(ctx context.Context, runID string) (bool, error) {
	run, err := s.store.GetWorkflowRun(ctx, runID)
	if err != nil {
		return false, err
	}
	if run.Status != store.RunRunning {
		return false, nil
	}
	nodes, err := s.store.RunNodes(ctx, runID)
	if err != nil {
		return false, err
	}
	workStates, workOutputs, workCount, err := s.flowWorkStates(ctx, runID)
	if err != nil {
		return false, err
	}
	// Execute ready scripts and switches outside the write transaction.
	results, ranScripts := s.executeFlowScripts(run, nodes)
	s.flowMu.Lock()
	startFails := make(map[string]string, len(s.flowStartFails))
	for id, msg := range s.flowStartFails {
		startFails[id] = msg
	}
	s.flowMu.Unlock()

	diff, err := flow.Evaluate(flow.Input{
		RunID: runID, Graph: run.Graph, RunStatus: run.Status,
		Nodes: nodes, WorkStates: workStates, WorkOutputs: workOutputs,
		ScriptResults: results, MaterializeFailures: startFails,
	})
	if err != nil {
		// An evaluation error is structural (bad graph state): fail the run
		// loudly rather than spinning on it every tick.
		return false, s.failRun(ctx, runID, err.Error())
	}
	if diff.Empty() && ranScripts == 0 {
		return false, nil
	}
	if len(diff.Starts) > 0 && workCount+len(diff.Starts) > store.MaxRunWorks {
		return false, s.failRun(ctx, runID, fmt.Sprintf("run exceeds %d works", store.MaxRunWorks))
	}
	s.expandStartObjectives(run, nodes, &diff)
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		return s.applyFlowDiff(ctx, tx, run, diff, ranScripts)
	})
	if err != nil {
		return false, err
	}
	return true, nil
}

// expandStartObjectives resolves {{run.*}} and {{steps.*}} references in the
// objectives of the Works about to start, against the node states this very
// diff produces (an upstream node may have finalized in it). Missing
// references expand to "" and are journaled with the diff.
func (s *Server) expandStartObjectives(run *store.WorkflowRun, nodes []store.RunNode, diff *flow.Diff) {
	if len(diff.Starts) == 0 {
		return
	}
	merged := make([]store.RunNode, len(nodes))
	copy(merged, nodes)
	byID := map[string]int{}
	for i, n := range merged {
		byID[n.ID] = i
	}
	for _, u := range diff.Updates {
		if i, ok := byID[u.Node.ID]; ok {
			merged[i] = u.Node
		}
	}
	merged = append(merged, diff.Creates...)
	for i := range diff.Starts {
		st := &diff.Starts[i]
		if st.Directive.Objective == "" {
			continue
		}
		input := flowScriptInput(run, merged, store.RunNode{NodeID: st.NodeID, Iteration: st.Iteration})
		expanded, missing := flow.ExpandTemplate(st.Directive.Objective, input)
		st.Directive.Objective = expanded
		if len(missing) > 0 {
			diff.Events = append(diff.Events, flow.Event{Kind: "workflow.template_missing", Payload: map[string]any{"node": st.NodeID, "iteration": st.Iteration, "references": missing}})
		}
	}
}

// flowWorkStates derives the state of every Work the run materialized and,
// for terminal ones, assembles the enriched node output from the latest
// attempt's result envelope: {state, summary, output} (per-target under
// `targets` when the Work spans repositories). `output` is the free-form
// object the agent put in its result's `output` field — the workflow
// data-passing contract.
func (s *Server) flowWorkStates(ctx context.Context, runID string) (map[string]model.WorkState, map[string]json.RawMessage, int, error) {
	works, err := s.store.WorkForRun(ctx, runID)
	if err != nil {
		return nil, nil, 0, err
	}
	ids := make([]string, len(works))
	for i, w := range works {
		ids[i] = w.ID
	}
	targets, err := s.store.TargetsForWorks(ctx, ids)
	if err != nil {
		return nil, nil, 0, err
	}
	states := make(map[string]model.WorkState, len(works))
	outputs := map[string]json.RawMessage{}
	var targetIDs []string
	for _, w := range works {
		states[w.ID] = model.DeriveWorkState(model.WorkInputs{Targets: engine.TargetStates(targets[w.ID]), Integrate: w.Integrate})
		if _, terminal := flow.NodeStatusForWork(states[w.ID]); terminal {
			for _, t := range targets[w.ID] {
				targetIDs = append(targetIDs, t.ID)
			}
		}
	}
	if len(targetIDs) == 0 {
		return states, outputs, len(works), nil
	}
	attempts, err := s.store.AttemptsForTargets(ctx, targetIDs)
	if err != nil {
		return nil, nil, 0, err
	}
	for _, w := range works {
		if _, terminal := flow.NodeStatusForWork(states[w.ID]); !terminal {
			continue
		}
		if out := flowNodeOutput(states[w.ID], targets[w.ID], attempts); out != nil {
			outputs[w.ID] = out
		}
	}
	return states, outputs, len(works), nil
}

// flowNodeOutput shapes one terminal Work into a node output. Single-target
// Works flatten summary/output to the top level; multi-target Works nest per
// repository. Oversized outputs are trimmed to state-only, flagged truncated.
func flowNodeOutput(state model.WorkState, ts []store.Target, attempts map[string][]store.Attempt) json.RawMessage {
	type targetOutput struct {
		State   string          `json:"state"`
		Summary string          `json:"summary,omitempty"`
		Output  json.RawMessage `json:"output,omitempty"`
	}
	perTarget := map[string]targetOutput{}
	for _, t := range ts {
		out := targetOutput{State: string(t.State)}
		list := attempts[t.ID]
		for i := len(list) - 1; i >= 0; i-- {
			env := decodeEnvelope(list[i].Result)
			if env == nil {
				continue
			}
			out.Summary = env.Summary
			out.Output = env.Extra["output"]
			break
		}
		perTarget[t.Repository] = out
	}
	body := map[string]any{"state": string(state)}
	if len(ts) == 1 {
		one := perTarget[ts[0].Repository]
		if one.Summary != "" {
			body["summary"] = one.Summary
		}
		if len(one.Output) > 0 {
			body["output"] = one.Output
		}
	} else if len(perTarget) > 0 {
		body["targets"] = perTarget
	}
	raw, err := json.Marshal(body)
	if err != nil {
		return nil
	}
	if len(raw) > store.MaxNodeOutputBytes {
		raw, err = json.Marshal(map[string]any{"state": string(state), "truncated": true})
		if err != nil {
			return nil
		}
	}
	return raw
}

// executeFlowScripts runs every ready script/switch instance under the run's
// script budget and returns their results for the evaluation.
func (s *Server) executeFlowScripts(run *store.WorkflowRun, nodes []store.RunNode) (map[string]flow.ScriptResult, int) {
	var results map[string]flow.ScriptResult
	ran := 0
	budget := run.ScriptRuns
	for _, inst := range nodes {
		if inst.Status != store.NodeReady || (inst.Type != store.NodeScript && inst.Type != store.NodeSwitch) {
			continue
		}
		if results == nil {
			results = map[string]flow.ScriptResult{}
		}
		if budget+ran >= store.MaxRunScripts {
			results[inst.ID] = flow.ScriptResult{Status: store.NodeFailed, Error: fmt.Sprintf("script budget exhausted (%d per run)", store.MaxRunScripts)}
			continue
		}
		results[inst.ID] = s.executeFlowScript(run, nodes, inst)
		ran++
	}
	return results, ran
}

// executeFlowScript runs one instance's body in the sandbox.
func (s *Server) executeFlowScript(run *store.WorkflowRun, nodes []store.RunNode, inst store.RunNode) flow.ScriptResult {
	def := run.Graph.Node(inst.NodeID)
	if def == nil {
		return flow.ScriptResult{Status: store.NodeFailed, Error: "node vanished from the graph"}
	}
	input := flowScriptInput(run, nodes, inst)
	switch inst.Type {
	case store.NodeSwitch:
		cfg, err := def.SwitchConfig()
		if err != nil {
			return flow.ScriptResult{Status: store.NodeFailed, Error: err.Error()}
		}
		value, err := flow.EvalSwitch(cfg.Expression, input, flow.ScriptTimeout(0))
		if err != nil {
			return flow.ScriptResult{Status: store.NodeFailed, Error: err.Error()}
		}
		out, err := json.Marshal(map[string]string{"case": value})
		if err != nil {
			return flow.ScriptResult{Status: store.NodeFailed, Error: err.Error()}
		}
		return flow.ScriptResult{Status: store.NodeSucceeded, Output: out}
	default:
		cfg, err := def.ScriptConfig()
		if err != nil {
			return flow.ScriptResult{Status: store.NodeFailed, Error: err.Error()}
		}
		source, timeoutMS := cfg.Source, cfg.TimeoutMS
		var interpreter []string
		path := ""
		if cfg.Script != "" {
			// A named script resolves from the LIVE library at execution (the
			// same freshness rule directive nodes have at materialization); a
			// script renamed mid-run fails the node cleanly.
			lib := s.promptLibrary()
			if lib == nil {
				return flow.ScriptResult{Status: store.NodeFailed, Error: "this process has no library to resolve script " + cfg.Script}
			}
			f := lib.Script(cfg.Script)
			if f == nil {
				return flow.ScriptResult{Status: store.NodeFailed, Error: fmt.Sprintf("script %q is not in the library (scripts/)", cfg.Script)}
			}
			source, interpreter, path = f.Body, f.Interpreter, f.Path
			if timeoutMS == 0 {
				timeoutMS = f.TimeoutMS
			}
			if len(cfg.Params) > 0 {
				input.Params = cfg.Params
			}
		}
		out, err := flow.RunAny(interpreter, path, source, input, timeoutMS)
		if err != nil {
			return flow.ScriptResult{Status: store.NodeFailed, Error: err.Error()}
		}
		return flow.ScriptResult{Status: store.NodeSucceeded, Output: out}
	}
}

// flowScriptInput assembles what a script sees: the run context and each
// node's latest terminal outcome.
func flowScriptInput(run *store.WorkflowRun, nodes []store.RunNode, inst store.RunNode) flow.ScriptInput {
	steps := map[string]flow.StepInput{}
	for _, n := range nodes {
		if !store.NodeTerminal(n.Status) {
			continue
		}
		if have, ok := steps[n.NodeID]; ok && have.Iteration >= n.Iteration {
			continue
		}
		step := flow.StepInput{Status: n.Status, Iteration: n.Iteration}
		if len(n.Output) > 0 {
			if n.Type == store.NodeDirective {
				// A routine node stores {state, summary, output, targets};
				// lift them so scripts read input.steps.x.output directly.
				var wrapped struct {
					State   string `json:"state"`
					Summary string `json:"summary"`
					Output  any    `json:"output"`
					Targets any    `json:"targets"`
				}
				if err := json.Unmarshal(n.Output, &wrapped); err == nil {
					step.State, step.Summary, step.Output, step.Targets = wrapped.State, wrapped.Summary, wrapped.Output, wrapped.Targets
				}
			} else {
				var out any
				if err := json.Unmarshal(n.Output, &out); err == nil {
					step.Output = out
				}
			}
		}
		steps[n.NodeID] = step
	}
	return flow.ScriptInput{
		Run: flow.RunInfo{
			ID: run.ID, Workflow: run.WorkflowName, Objective: run.Context.Objective,
			Repositories: run.Context.Repositories, Trigger: string(run.Trigger), Iteration: inst.Iteration,
		},
		Steps: steps,
	}
}

// applyFlowDiff writes one evaluation's transitions in a single transaction:
// instance rows, Work materializations, script accounting, journal rows, and
// the run status.
func (s *Server) applyFlowDiff(ctx context.Context, tx *store.Tx, run *store.WorkflowRun, diff flow.Diff, ranScripts int) error {
	byKey := map[string]*store.RunNode{}
	for i := range diff.Creates {
		n := &diff.Creates[i]
		n.RunID = run.ID
		if store.NodeTerminal(n.Status) {
			n.FinishedAt = tx.Now()
		}
		if err := tx.CreateRunNode(ctx, n); err != nil {
			return err
		}
		byKey[n.NodeID+"#"+fmt.Sprint(n.Iteration)] = n
	}
	for i := range diff.Updates {
		u := &diff.Updates[i]
		if store.NodeTerminal(u.Node.Status) && u.Node.FinishedAt.IsZero() {
			u.Node.FinishedAt = tx.Now()
		}
		if err := tx.UpdateRunNodeFrom(ctx, &u.Node, u.From); err != nil {
			return err
		}
		if store.NodeTerminal(u.Node.Status) {
			if err := tx.Journal(ctx, "workflow.node_finished", store.EntityWorkflow, run.ID, map[string]any{"node": u.Node.NodeID, "iteration": u.Node.Iteration, "status": u.Node.Status}); err != nil {
				return err
			}
		}
		byKey[u.Node.NodeID+"#"+fmt.Sprint(u.Node.Iteration)] = &u.Node
	}
	for _, st := range diff.Starts {
		err := s.startFlowWork(ctx, tx, run, st, byKey)
		if err == nil {
			continue
		}
		// A definitional mistake — an unregistered repository, an archived
		// routine, a bad model alias — would fail this transaction forever.
		// Record it and leave the instance ready: the next evaluation fails
		// the node through the engine, so its tokens still deliver and the
		// skip cascade and run status say what happened. Infrastructure
		// errors still abort and retry.
		var re *requestError
		if !errors.As(err, &re) && !errors.Is(err, store.ErrNotFound) && !errors.Is(err, store.ErrConflict) {
			return err
		}
		inst := byKey[st.NodeID+"#"+fmt.Sprint(st.Iteration)]
		if inst == nil {
			return err
		}
		s.flowMu.Lock()
		if s.flowStartFails == nil {
			s.flowStartFails = map[string]string{}
		}
		s.flowStartFails[inst.ID] = err.Error()
		s.flowMu.Unlock()
		if jerr := tx.Journal(ctx, "workflow.node_failed", store.EntityWorkflow, run.ID, map[string]any{"node": st.NodeID, "iteration": st.Iteration, "error": err.Error()}); jerr != nil {
			return jerr
		}
	}
	if ranScripts > 0 {
		if err := tx.AddWorkflowRunScripts(ctx, run.ID, ranScripts); err != nil {
			return err
		}
	}
	for _, ev := range diff.Events {
		if err := tx.Journal(ctx, ev.Kind, store.EntityWorkflow, run.ID, ev.Payload); err != nil {
			return err
		}
	}
	if diff.RunStatus != "" && diff.RunStatus != run.Status {
		if err := tx.SetWorkflowRunStatus(ctx, run.ID, diff.RunStatus); err != nil {
			return err
		}
		if err := tx.Journal(ctx, "workflow.run_finished", store.EntityWorkflow, run.ID, map[string]any{"workflow": run.WorkflowName, "status": diff.RunStatus}); err != nil {
			return err
		}
	}
	return nil
}

// startFlowWork materializes one ready directive instance into a Work and
// flips the instance to running, in the same transaction.
func (s *Server) startFlowWork(ctx context.Context, tx *store.Tx, run *store.WorkflowRun, st flow.Start, byKey map[string]*store.RunNode) error {
	req := workRequest{
		directive:    st.Directive.Directive,
		Repositories: st.Directive.Repositories,
		Objective:    st.Directive.Objective,
		Persona:      st.Directive.Persona,
		Model:        st.Directive.Model,
		Class:        st.Directive.BudgetClass,
		nodeTimeout:  st.Directive.TimeoutSeconds,
		nodeMaxTurns: st.Directive.MaxTurns,
		Integrate:    st.Directive.Integrate,
	}
	if len(req.Repositories) == 0 {
		req.Repositories = run.Context.Repositories
	}
	if req.Objective == "" {
		req.Objective = run.Context.Objective
	}
	step := st.NodeID
	if st.Iteration > 1 {
		step = fmt.Sprintf("%s#%d", st.NodeID, st.Iteration)
	}
	req.Title = run.WorkflowName + ": " + step
	req.workflowRunID, req.workflowName, req.workflowStep = run.ID, run.WorkflowName, step
	req.stepEdges, req.trigger = st.BlockedBy, run.Trigger
	created, err := s.createWorkTx(ctx, tx, req)
	if err != nil {
		return fmt.Errorf("node %s: %w", st.NodeID, err)
	}
	inst := byKey[st.NodeID+"#"+fmt.Sprint(st.Iteration)]
	if inst == nil {
		// The instance was ready in the database already (a re-advance after
		// a partial apply): load and flip it.
		nodes, err := tx.RunNodes(ctx, run.ID)
		if err != nil {
			return err
		}
		for i := range nodes {
			if nodes[i].NodeID == st.NodeID && nodes[i].Iteration == st.Iteration {
				inst = &nodes[i]
				break
			}
		}
		if inst == nil {
			return fmt.Errorf("node %s#%d: instance vanished", st.NodeID, st.Iteration)
		}
	}
	inst.WorkID = created.Work.ID
	inst.StartedAt = tx.Now()
	from := inst.Status
	inst.Status = store.NodeRunning
	if err := tx.UpdateRunNodeFrom(ctx, inst, from); err != nil {
		return err
	}
	return tx.Journal(ctx, "workflow.node_started", store.EntityWorkflow, run.ID, map[string]any{"node": st.NodeID, "iteration": st.Iteration, "work_id": created.Work.ID})
}

// failRun marks a run failed with a reason — the guardrail path.
func (s *Server) failRun(ctx context.Context, runID, reason string) error {
	return s.store.Write(ctx, func(tx *store.Tx) error {
		run, err := tx.GetWorkflowRun(ctx, runID)
		if err != nil {
			return err
		}
		if run.Status != store.RunRunning {
			return nil
		}
		if err := tx.SetWorkflowRunStatus(ctx, runID, store.RunFailed); err != nil {
			return err
		}
		return tx.Journal(ctx, "workflow.run_failed", store.EntityWorkflow, runID, map[string]any{"reason": reason})
	})
}
