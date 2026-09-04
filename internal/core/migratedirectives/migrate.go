// Package migratedirectives is the one-time (in effect) restructure that
// turns stored routines into trigger shells: the library directory renames
// prompts→directives, every content routine's prompt splits into
// directives/<name>.md with the row re-pointed at it, and workflow graphs
// convert routine nodes to directive nodes with the routine's operational
// envelope baked in.
//
// Idempotence is state-derived, never flag-derived: every step's predicate is
// a state check (the new dir exists / target = ” / a graph contains routine
// nodes), so Run is safe on every daemon boot forever — "once" is emergent.
// Crash-resumability rides the same predicates: a file written before its row
// update is rewritten byte-identically on the next boot; a row updated is
// skipped by the target = ” predicate.
package migratedirectives

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"

	"forge/internal/core/prompts"
	"forge/internal/core/store"
)

// Report is what one Run did (or, dry, would do).
type Report struct {
	DirRenamed      bool              `json:"dir_renamed,omitempty"`
	RoutinesSplit   []string          `json:"routines_split,omitempty"`
	GraphsConverted []string          `json:"graphs_converted,omitempty"`
	Skipped         map[string]string `json:"skipped,omitempty"` // name → reason
}

// Empty reports a no-op run (nothing to migrate — the steady state).
func (r Report) Empty() bool {
	return !r.DirRenamed && len(r.RoutinesSplit) == 0 && len(r.GraphsConverted) == 0 && len(r.Skipped) == 0
}

// Run migrates one home. home is the forge home (for the old default
// <home>/prompts); libDir is the configured library path — the rename fires
// only when libDir is the new default <home>/directives, so a custom path is
// never touched. dryRun reports without writing anything.
func Run(ctx context.Context, st *store.Store, home, libDir string, dryRun bool, log *slog.Logger) (Report, error) {
	rep := Report{Skipped: map[string]string{}}

	// Step 1: the directory rename, atomic on one filesystem; .git moves with
	// it, history preserved.
	oldDir := filepath.Join(home, "prompts")
	if libDir == filepath.Join(home, "directives") {
		_, newErr := os.Stat(libDir)
		_, oldErr := os.Stat(oldDir)
		switch {
		case newErr == nil && oldErr == nil:
			log.WarnContext(ctx, "both prompts and directives dirs exist; using directives, leaving prompts untouched", "old", oldDir, "new", libDir)
			rep.Skipped["dir:"+oldDir] = "both directories exist"
		case newErr == nil:
			// Renamed already (or a fresh install created directives): done.
		case oldErr == nil:
			if !dryRun {
				if err := os.Rename(oldDir, libDir); err != nil {
					return rep, fmt.Errorf("rename %s → %s: %w", oldDir, libDir, err)
				}
			}
			rep.DirRenamed = true
		}
	}
	if !dryRun {
		if err := prompts.Ensure(libDir); err != nil {
			return rep, fmt.Errorf("ensure %s: %w", libDir, err)
		}
	}

	// Step 2: split content routines. File first, row second — each side's
	// predicate makes any crash point re-run cleanly.
	routines, err := st.ListRoutines(ctx, false)
	if err != nil {
		return rep, err
	}
	split := false
	for _, rt := range routines {
		if rt.Target != "" || rt.Prompt == "" {
			continue
		}
		if strings.Contains(rt.Prompt, "{{>") {
			// In a stored routine prompt this was always literal text; in a
			// directive file it becomes include syntax (expanding, or breaking
			// the load). Leave such a routine legacy rather than change its
			// meaning.
			rep.Skipped[rt.Name] = "prompt contains literal {{> include syntax; split by hand"
			continue
		}
		path := filepath.Join(libDir, "directives", rt.Name+".md")
		content := directiveFile(&rt)
		if existing, err := os.ReadFile(path); err == nil && string(existing) != content {
			// A hand-authored directive of different content owns the name;
			// the routine stays legacy (dual-mode keeps it running).
			rep.Skipped[rt.Name] = "directives/" + rt.Name + ".md exists with different content"
			continue
		}
		for _, clash := range []string{"fragments", "personas"} {
			if _, err := os.Stat(filepath.Join(libDir, clash, rt.Name+".md")); err == nil {
				rep.Skipped[rt.Name] = clash + "/" + rt.Name + ".md would collide in the library namespace"
			}
		}
		if _, clashed := rep.Skipped[rt.Name]; clashed {
			continue
		}
		if dryRun {
			rep.RoutinesSplit = append(rep.RoutinesSplit, rt.Name)
			continue
		}
		if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
			return rep, fmt.Errorf("write %s: %w", path, err)
		}
		err := st.Write(ctx, func(tx *store.Tx) error {
			saved, err := tx.GetRoutine(ctx, rt.Name)
			if err != nil || saved.Target != "" {
				return err // re-read predicate: someone else already split it
			}
			next := *saved
			next.Target = "directive:" + rt.Name
			next.Mode, next.Prompt, next.Persona, next.Model, next.Effort = "", "", "", "", ""
			// The generation bump snapshots the last content-ful state into
			// routine_generations — the pre-split content stays recoverable.
			return tx.UpdateRoutineFrom(ctx, &next, saved.Generation, "migrate:directives")
		})
		if err != nil {
			return rep, fmt.Errorf("split routine %s: %w", rt.Name, err)
		}
		rep.RoutinesSplit = append(rep.RoutinesSplit, rt.Name)
		split = true
	}
	if split {
		commitAll(libDir)
	}

	// The split files must compose before graphs point at them.
	if !dryRun {
		if _, err := prompts.Load(libDir); err != nil {
			return rep, fmt.Errorf("library does not load after the split: %w", err)
		}
	}

	// Step 3: convert workflow routine nodes whose routine now targets a
	// directive. Frozen run graphs (workflow_runs.graph) are never rewritten.
	// A dry run's rows are still unsplit; the would-split set stands in so
	// the report matches what a wet run will do.
	willSplit := map[string]bool{}
	if dryRun {
		for _, name := range rep.RoutinesSplit {
			willSplit[name] = true
		}
	}
	byName := map[string]*store.Routine{}
	if fresh, err := st.ListRoutines(ctx, false); err == nil {
		for i := range fresh {
			byName[fresh[i].Name] = &fresh[i]
		}
	}
	workflows, err := st.ListWorkflows(ctx, false)
	if err != nil {
		return rep, err
	}
	for _, wf := range workflows {
		if wf.Graph == nil {
			continue
		}
		converted, ok := convertGraph(wf.Graph, byName, willSplit, &rep)
		if !ok {
			continue
		}
		if dryRun {
			rep.GraphsConverted = append(rep.GraphsConverted, wf.Name)
			continue
		}
		next := wf
		next.Graph = converted
		err := st.Write(ctx, func(tx *store.Tx) error {
			return tx.UpdateWorkflowFrom(ctx, &next, wf.Generation, "migrate:directives")
		})
		if err != nil {
			return rep, fmt.Errorf("convert workflow %s: %w", wf.Name, err)
		}
		rep.GraphsConverted = append(rep.GraphsConverted, wf.Name)
	}

	if len(rep.Skipped) == 0 {
		rep.Skipped = nil
	}
	if !rep.Empty() && !dryRun {
		err := st.Write(ctx, func(tx *store.Tx) error {
			return tx.Journal(ctx, "daemon.migrated_directives", store.EntityDaemon, "directives", map[string]any{
				"dir_renamed": rep.DirRenamed, "routines_split": len(rep.RoutinesSplit),
				"graphs_converted": len(rep.GraphsConverted), "skipped": len(rep.Skipped),
			})
		})
		if err != nil {
			log.WarnContext(ctx, "journal directives migration", "error", err)
		}
	}
	return rep, nil
}

// directiveFile renders the split file: frontmatter from the routine's
// content settings (mode always; the empty ones omitted), body = the prompt
// verbatim. Deterministic, so a crashed half-run rewrites identical bytes.
func directiveFile(rt *store.Routine) string {
	var b strings.Builder
	b.WriteString("---\nmode: " + rt.Mode + "\n")
	if rt.Persona != "" {
		b.WriteString("persona: " + rt.Persona + "\n")
	}
	if rt.Model != "" {
		b.WriteString("model: " + rt.Model + "\n")
	}
	if rt.Effort != "" {
		b.WriteString("effort: " + rt.Effort + "\n")
	}
	b.WriteString("---\n")
	b.WriteString(strings.TrimRight(rt.Prompt, "\n"))
	b.WriteString("\n")
	return b.String()
}

// convertGraph rewrites routine nodes to directive nodes where the routine
// now targets the same-named directive (or, on a dry run, would after the
// split), baking the routine's operational envelope so behavior is unchanged
// (the model rides the directive's frontmatter, written by the split).
// Returns (nil, false) when nothing converts. Dangling or still-legacy
// routine references are left alone and reported once.
func convertGraph(g *store.WorkflowGraph, byName map[string]*store.Routine, willSplit map[string]bool, rep *Report) (*store.WorkflowGraph, bool) {
	next := *g
	next.Nodes = append([]store.WorkflowNode(nil), g.Nodes...)
	changed := false
	for i, n := range next.Nodes {
		if n.Type != store.NodeRoutine {
			continue
		}
		cfg, err := n.RoutineConfig()
		if err != nil {
			continue
		}
		rt := byName[cfg.Routine]
		if rt == nil {
			rep.Skipped["node:"+cfg.Routine] = "routine node references a routine that no longer exists"
			continue
		}
		kind, dname, err := store.ParseTarget(rt.Target)
		if (err != nil || kind != store.TargetDirective || dname != rt.Name) && !willSplit[rt.Name] {
			rep.Skipped["node:"+cfg.Routine] = "routine is not a same-named directive target"
			continue
		}
		config := map[string]any{"directive": rt.Name}
		if len(cfg.Repositories) > 0 {
			config["repositories"] = cfg.Repositories
		}
		if cfg.Objective != "" {
			config["objective"] = cfg.Objective
		}
		if cfg.Persona != "" {
			config["persona"] = cfg.Persona
		}
		if rt.TimeoutSeconds > 0 {
			config["timeout_seconds"] = rt.TimeoutSeconds
		}
		if rt.MaxTurns > 0 {
			config["max_turns"] = rt.MaxTurns
		}
		if rt.BudgetClass != "" {
			config["budget_class"] = string(rt.BudgetClass)
		}
		next.Nodes[i] = store.WorkflowNode{ID: n.ID, Type: store.NodeDirective, Config: config, Position: n.Position}
		changed = true
	}
	if !changed {
		return nil, false
	}
	return &next, true
}

// commitAll is the best-effort library commit after the split — the
// bootstrapCommit posture: where git balks the tree stays dirty, which the
// next manifest records honestly, and the next boot's run commits it.
func commitAll(dir string) {
	prompts.CommitEdit(dir, ".", "forge: split routine prompts into directives")
}
