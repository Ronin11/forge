// Package plan implements the plan mode: a goal and a repository become a DAG
// of tasks with write sets, dependency edges, and stacking hints, created as
// a batch by the daemon (DESIGN.md §20, MODES.md).
package plan

import (
	_ "embed"
	"encoding/json"

	"forge/internal/core/model"
	"forge/internal/core/modes"
	"forge/internal/core/modes/schema"
	"forge/internal/core/protocol"
)

//go:embed preamble.md
var preamble string

// extras are plan's result fields beyond the envelope: the shared task-array
// fragment (schema.PlanTasks — supervise emits the same shape).
const extras = `{"tasks":` + schema.PlanTasks + `}`

// New returns the plan mode.
func New() modes.Mode { return mode{} }

// mode answers the Mode contract with plan's constants (DESIGN.md §20).
type mode struct{}

func (mode) Name() string     { return "plan" }
func (mode) Preamble() string { return preamble }

// AllowedTools is the read toolset: plan must see the repository but never
// change it — the preamble forbids writes and L0's WritesNone scope enforces
// it against git afterwards.
func (mode) AllowedTools() []string {
	return []string{
		"Read", "Grep", "Glob", "Bash",
		"forge_repo_status", "forge_note_progress", "forge_kb_search", "forge_usage",
		"forge_library", "forge_script_run",
	}
}

func (mode) ResultSchema() json.RawMessage { return schema.MustExtend(extras, "tasks") }

func (mode) Level() model.VerificationLevel  { return model.L0 }
func (mode) Checkpoints() []string           { return []string{"before_report"} }
func (mode) DefaultClass() model.BudgetClass { return model.ClassInteractive }

// DefaultAutonomy is empty: plan inherits the project default (checkpoint
// when nothing sets one — ResolveAutonomy's fallback).
func (mode) DefaultAutonomy() model.Autonomy { return "" }

func (mode) Writes() model.WriteScope { return model.WritesNone }

// FollowUps spawns nothing here: the batch needs per-task paths, dependency
// edges, stack hints, and a shared plan_batch_id, which WorkSpec does not
// carry — the daemon creates it from the parsed result (planFollowUps,
// controlplane/handlers_plan.go).
func (mode) FollowUps(*protocol.ResultEnvelope, modes.FollowUpContext) []modes.WorkSpec {
	return nil
}
