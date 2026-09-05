// Package supervise implements the supervise mode: the return path of
// recursive decomposition. A continuation Work in this mode fires when a
// plan batch settles; the agent reviews what the children actually produced
// (forge_work_outcomes) and either declares the goal met — with 1-5 scores —
// or emits corrective tasks, which the daemon fans out as the next round
// (handlers_supervise.go; DESIGN.md §20).
package supervise

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

// extras: a required assessment (outcome + full scores + weakness) and an
// optional tasks array in plan's exact shape — required daemon-side when
// outcome is revise.
const extras = `{` +
	`"assessment":{"type":"object","additionalProperties":false,"required":["outcome","scores","weakness"],"properties":{` +
	`"outcome":{"type":"string","enum":["done","revise"]},` +
	`"scores":{"type":"object","additionalProperties":false,"required":["correctness","completeness","quality","effort_fit","overall"],"properties":{` +
	`"correctness":{"type":"integer","minimum":1,"maximum":5},` +
	`"completeness":{"type":"integer","minimum":1,"maximum":5},` +
	`"quality":{"type":"integer","minimum":1,"maximum":5},` +
	`"effort_fit":{"type":"integer","minimum":1,"maximum":5},` +
	`"overall":{"type":"integer","minimum":1,"maximum":5}}},` +
	`"weakness":{"type":"string"}}},` +
	`"tasks":` + schema.PlanTasks + `}`

// New returns the supervise mode.
func New() modes.Mode { return mode{} }

type mode struct{}

func (mode) Name() string     { return "supervise" }
func (mode) Preamble() string { return preamble }

// AllowedTools is the read toolset plus the outcomes tool: the supervisor
// inspects the repository and its batch but never patches what it scores —
// fixes flow through corrective tasks.
func (mode) AllowedTools() []string {
	return []string{
		"Read", "Grep", "Glob", "Bash",
		"forge_work_outcomes", "forge_repo_status", "forge_note_progress",
		"forge_kb_search", "forge_kb_new", "forge_usage",
	}
}

func (mode) ResultSchema() json.RawMessage { return schema.MustExtend(extras, "assessment") }

func (mode) Level() model.VerificationLevel  { return model.L0 }
func (mode) Checkpoints() []string           { return []string{"before_report"} }
func (mode) DefaultClass() model.BudgetClass { return model.ClassInteractive }
func (mode) DefaultAutonomy() model.Autonomy { return "" }

func (mode) Writes() model.WriteScope { return model.WritesKbOnly }

// FollowUps spawns nothing here: the revise batch needs per-task paths,
// edges, and a shared plan_batch_id, which WorkSpec does not carry — the
// daemon creates it (superviseFollowUps, handlers_supervise.go).
func (mode) FollowUps(*protocol.ResultEnvelope, modes.FollowUpContext) []modes.WorkSpec {
	return nil
}
