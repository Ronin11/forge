package web

// The daemon side of the tool/skill bridge: the closures forge_directive_run
// and forge_workflow_run reach through tools.Deps. This is where the
// guardrails live — since an empty AllowedTools list exposes every tool to
// every agent, safety is enforced at the call, not the listing: tool-flag
// checks happen in the tools, and here the spawn depth cap, the per-work
// spawn cap, the class ceiling, and the provenance stamps
// (cause=tool, submitted_by=agent:<attempt>, caused_by=the caller's work).

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"forge/internal/core/directives"
	"forge/internal/core/flow"
	"forge/internal/core/model"
	"forge/internal/core/store"
	"forge/internal/tools"
)

const (
	// maxToolSpawnDepth: a spawned Work's agent may not spawn further —
	// depth counts consecutive cause=tool links in the caused_by chain.
	maxToolSpawnDepth = 1
	// maxToolSpawnsPerWork bounds fan-out from one task.
	maxToolSpawnsPerWork = 5
	// spawnAncestryBound stops a corrupt caused_by chain from looping.
	spawnAncestryBound = 10
)

// spawnWorkForTool is Deps.SpawnWork: guardrails, then the ordinary
// directive materialization path of createWorkTx.
func (s *Server) spawnWorkForTool(ctx context.Context, att tools.Attempt, in tools.SpawnInput) (string, error) {
	if in.Class == "" {
		in.Class = model.ClassBacklog
	}
	if in.Class == model.ClassInteractive {
		return "", tools.BadInput("class interactive is reserved for humans")
	}
	var out workCreated
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		// Depth cap: walk the caused_by chain counting tool links.
		depth := 0
		workID := att.WorkID
		for hop := 0; hop < spawnAncestryBound && workID != ""; hop++ {
			w, err := tx.GetWork(ctx, workID)
			if err != nil {
				return err
			}
			if w.Cause != model.CauseTool {
				break
			}
			depth++
			workID = w.CausedByWorkID
		}
		if depth >= maxToolSpawnDepth {
			return tools.BadInput("spawned work may not spawn further (depth %d): finish the task yourself or ask a human", depth)
		}
		// Fan-out cap per calling work.
		n, err := tx.CountToolSpawns(ctx, att.WorkID)
		if err != nil {
			return err
		}
		if n >= maxToolSpawnsPerWork {
			return tools.BadInput("this task already spawned %d sub-tasks (the cap): work with what you have", n)
		}
		created, err := s.createWorkTx(ctx, tx, workRequest{
			directive:    in.Directive,
			Objective:    in.Objective,
			Repositories: in.Repositories,
			Class:        in.Class,
			CausedBy:     att.WorkID,
			// The child never runs more autonomously than its parent.
			Autonomy:    att.Autonomy,
			cause:       model.CauseTool,
			submittedBy: "agent:" + att.ID,
		})
		if err != nil {
			return err
		}
		out = created
		return tx.Journal(ctx, "tool.spawned_work", store.EntityWork, out.Work.ID, map[string]any{
			"attempt": att.ID, "parent_work": att.WorkID, "directive": in.Directive, "class": in.Class,
		})
	})
	if err != nil {
		return "", err
	}
	s.log.InfoContext(ctx, "agent spawned work", "attempt", att.ID, "parent_work", att.WorkID, "work_id", out.Work.ID, "directive", in.Directive)
	return out.Work.ID, nil
}

// startWorkflowRunForTool is Deps.StartWorkflowRun: the runWorkflow shape
// with tool provenance journaled. The tool already checked the flag; the row
// is re-read inside the tx so a race with archive/un-flag still refuses.
func (s *Server) startWorkflowRunForTool(ctx context.Context, att tools.Attempt, workflow, objective string, repos []string) (string, error) {
	run := &store.WorkflowRun{Trigger: model.TriggerManual, Context: store.RunContext{Repositories: repos, Objective: objective}}
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		wf, err := tx.GetWorkflow(ctx, workflow)
		if err != nil {
			return err
		}
		if !wf.ArchivedAt.IsZero() || !wf.Tool {
			return tools.BadInput("workflow %q is not callable", workflow)
		}
		run.WorkflowID, run.WorkflowName, run.WorkflowGeneration, run.Graph = wf.ID, wf.Name, wf.Generation, wf.Graph
		if err := tx.CreateWorkflowRun(ctx, run); err != nil {
			return err
		}
		return tx.Journal(ctx, "tool.workflow_run", store.EntityWorkflow, run.ID, map[string]any{
			"attempt": att.ID, "parent_work": att.WorkID, "workflow": workflow,
		})
	})
	if err != nil {
		return "", err
	}
	s.advanceRun(ctx, run.ID)
	s.log.InfoContext(ctx, "agent fired workflow run", "attempt", att.ID, "workflow", workflow, "run_id", run.ID)
	return run.ID, nil
}

// scratchDefaults fills zero config (bare test servers).
func (s *Server) scratchLimits() (max, promoteRuns, promoteAttempts int) {
	max, promoteRuns, promoteAttempts = s.scratchCfg.Max, s.scratchCfg.PromoteRuns, s.scratchCfg.PromoteAttempts
	if max <= 0 {
		max = 200
	}
	if promoteRuns <= 0 {
		promoteRuns = 5
	}
	if promoteAttempts <= 0 {
		promoteAttempts = 2
	}
	return max, promoteRuns, promoteAttempts
}

// scratchForTool is Deps.Scratch: save (optional), run, count, and — past
// the threshold — promote into the git library. The organic layer: agents
// reach for quick scripts through here; what keeps getting reached for
// stops being ephemeral.
func (s *Server) scratchForTool(ctx context.Context, att tools.Attempt, in tools.ScratchInput) (json.RawMessage, tools.ScratchMeta, error) {
	var meta tools.ScratchMeta
	lib := s.promptLibrary()
	if lib != nil && lib.Fragment(in.Name) != nil {
		return nil, meta, tools.BadInput("%q is a library name — run it via forge_script_run (or pick another scratch name)", in.Name)
	}
	max, promoteRuns, promoteAttempts := s.scratchLimits()

	// Save (or replace) when source is given; always load + touch.
	var row *store.ScratchScript
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		if in.Source != "" {
			sc := &store.ScratchScript{Name: in.Name, Source: in.Source, Language: in.Language,
				Description: in.Description, InputSchema: in.InputSchema, CreatedBy: "agent:" + att.ID}
			if att.ID == "" {
				sc.CreatedBy = "human"
			}
			if err := tx.UpsertScratch(ctx, sc, max); err != nil {
				return err
			}
			if err := tx.Journal(ctx, "scratch.saved", store.EntityDaemon, in.Name, map[string]any{"attempt": att.ID, "language": in.Language, "hash": sc.Hash}); err != nil {
				return err
			}
		}
		var err error
		row, err = tx.TouchScratchRun(ctx, in.Name, att.ID)
		return err
	})
	if err != nil {
		return nil, meta, err
	}

	// Execute: js in the goja sandbox, anything else as a subprocess from
	// the materialized cache dir.
	input := flow.ScriptInput{Params: in.Input}
	var out json.RawMessage
	if row.Language == "js" {
		out, err = flow.RunScript(row.Source, input, flow.ScriptTimeout(0))
	} else {
		dir := filepath.Join(s.home, "scratch-scripts")
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return nil, meta, err
		}
		path := filepath.Join(dir, row.Name+"."+row.Language)
		if err := os.WriteFile(path, []byte(row.Source), 0o644); err != nil {
			return nil, meta, err
		}
		var interp []string
		interp, err = directives.ResolveInterpreter(row.Source, path)
		if err != nil {
			return nil, meta, tools.BadInput("%v", err)
		}
		out, err = flow.RunExternal(interp, path, input, flow.ExternalTimeout(0))
	}
	if err != nil {
		return nil, meta, tools.BadInput("scratch %s: %v", row.Name, err)
	}
	meta.RunCount = row.RunCount

	// Promotion: enough runs across enough distinct attempts → the script
	// stops being ephemeral and joins the git library, fully automatically
	// (the operator's explicit choice; the journal and git history are the
	// audit trail).
	if row.RunCount >= promoteRuns && len(row.Attempts) >= promoteAttempts {
		if err := s.promoteScratch(ctx, row); err != nil {
			s.log.WarnContext(ctx, "scratch promotion", "script", row.Name, "error", err)
		} else {
			meta.Promoted = true
		}
	}
	return out, meta, nil
}

// promoteScratch writes the row into the library's scripts/ tree with a
// synthesized metadata header (tool-flagged when it carries an input
// schema), validates the whole library, commits, hot-reloads, and drops the
// cache row. Any failure reverts the file and keeps the row — the next run
// retries.
func (s *Server) promoteScratch(ctx context.Context, row *store.ScratchScript) error {
	lib := s.promptLibrary()
	if lib == nil {
		return fmt.Errorf("no library to promote into")
	}
	if lib.Fragment(row.Name) != nil {
		return fmt.Errorf("library name %q is taken", row.Name)
	}
	path := filepath.Join(lib.Dir, "scripts", row.Name+"."+row.Language)
	content := synthesizeScriptFile(row)
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		return err
	}
	if _, err := directives.Load(lib.Dir); err != nil {
		if rerr := os.Remove(path); rerr != nil {
			s.log.ErrorContext(ctx, "revert failed promotion", "path", path, "error", rerr)
		}
		return fmt.Errorf("promoted file does not load: %w", err)
	}
	directives.CommitEdit(lib.Dir, path, fmt.Sprintf("scratch: promote %s (%d runs, %d attempts, by %s)", row.Name, row.RunCount, len(row.Attempts), row.CreatedBy))
	if s.promptsReload != nil {
		if err := s.promptsReload(); err != nil {
			s.log.WarnContext(ctx, "reload after promotion", "error", err)
		}
	}
	err := s.store.Write(ctx, func(tx *store.Tx) error {
		if err := tx.DeleteScratch(ctx, row.Name); err != nil {
			return err
		}
		return tx.Journal(ctx, "scratch.promoted", store.EntityDaemon, row.Name, map[string]any{
			"language": row.Language, "runs": row.RunCount, "attempts": len(row.Attempts), "created_by": row.CreatedBy, "tool": row.InputSchema != "",
		})
	})
	if err != nil {
		return err
	}
	s.log.InfoContext(ctx, "scratch script promoted to the library", "script", row.Name, "runs", row.RunCount, "created_by", row.CreatedBy)
	return nil
}

// synthesizeScriptFile renders the promoted file: the metadata header the
// library expects, from the scratch row's fields, above the source verbatim.
// tool: true only when a schema exists — the library's own rule.
func synthesizeScriptFile(row *store.ScratchScript) string {
	desc := strings.Join(strings.Fields(row.Description), " ")
	compactSchema := ""
	if row.InputSchema != "" {
		var buf bytes.Buffer
		if json.Compact(&buf, []byte(row.InputSchema)) == nil {
			compactSchema = buf.String()
		}
	}
	if row.Language == "js" {
		var b strings.Builder
		b.WriteString("/**forge\n * description: " + desc + "\n")
		if compactSchema != "" {
			b.WriteString(" * input: " + compactSchema + "\n * tool: true\n")
		}
		b.WriteString(" */\n")
		b.WriteString(row.Source)
		if !strings.HasSuffix(row.Source, "\n") {
			b.WriteString("\n")
		}
		return b.String()
	}
	source := row.Source
	shebang := ""
	if strings.HasPrefix(source, "#!") {
		if i := strings.IndexByte(source, '\n'); i >= 0 {
			shebang, source = source[:i+1], source[i+1:]
		}
	}
	var b strings.Builder
	b.WriteString(shebang)
	b.WriteString("#forge\n# description: " + desc + "\n")
	if compactSchema != "" {
		b.WriteString("# input: " + compactSchema + "\n# tool: true\n")
	}
	b.WriteString(source)
	if !strings.HasSuffix(source, "\n") {
		b.WriteString("\n")
	}
	return b.String()
}
