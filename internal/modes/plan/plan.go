// Package plan implements the plan mode: a goal and a repository become a DAG
// of tasks with write sets, dependency edges, and stacking hints, created as
// a batch by the daemon (DESIGN.md §20, MODES.md).
package plan

import (
	_ "embed"
	"encoding/json"

	"forge/internal/model"
	"forge/internal/modes"
	"forge/internal/modes/schema"
	"forge/internal/protocol"
)

//go:embed preamble.md
var preamble string

// extras are plan's result fields beyond the envelope. blocked_by entries are
// indexes into the same tasks array; the daemon turns them into edges.
const extras = `{` +
	`"tasks":{"type":"array","minItems":1,"items":{"type":"object","additionalProperties":false,"required":["title","prompt","paths"],"properties":{` +
	`"title":{"type":"string"},` +
	`"prompt":{"type":"string"},` +
	`"paths":{"type":"array","items":{"type":"string"}},` +
	`"blocked_by":{"type":"array","items":{"type":"integer","minimum":0}},` +
	`"stack_on":{"type":"boolean"},` +
	`"size":{"type":"string","enum":["S","M","L"]},` +
	`"tier":{"type":"integer","minimum":0,"maximum":3}}}}}`

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
