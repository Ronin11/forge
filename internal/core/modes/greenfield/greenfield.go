// Package greenfield implements the greenfield mode: interview → spec → plan
// → build → verify → report, in an owned directory Forge moves to
// projects_root on completion (MODES.md §greenfield, recommended design).
package greenfield

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

// extras are greenfield's result fields beyond the envelope; project_name
// names the slug Forge moves the directory to.
const extras = `{` +
	`"spec_note":{"type":"string"},` +
	`"plan":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["step","done"],"properties":{"step":{"type":"string"},"done":{"type":"boolean"},"evidence":{"type":"string"}}}},` +
	`"project_path":{"type":"string"},` +
	`"project_name":{"type":"string"}}`

// New returns the greenfield mode.
func New() modes.Mode { return mode{} }

// mode answers the Mode contract with greenfield's constants from MODES.md.
type mode struct{}

func (mode) Name() string     { return "greenfield" }
func (mode) Preamble() string { return preamble }

func (mode) AllowedTools() []string {
	return append(modes.Builtins(),
		"forge_kb_new", "forge_kb_note", "forge_check",
		"forge_note_progress", "forge_kb_search", "forge_usage",
		"forge_library", "forge_script_run")
}

func (mode) ResultSchema() json.RawMessage { return schema.MustExtend(extras) }

func (mode) Level() model.VerificationLevel { return model.L2 }

func (mode) Checkpoints() []string {
	return []string{"after_spec", "after_plan", "before_report"}
}

func (mode) DefaultClass() model.BudgetClass { return model.ClassNormal }
func (mode) DefaultAutonomy() model.Autonomy { return model.AutonomyCheckpoint }
func (mode) Writes() model.WriteScope        { return model.WritesNewProject }

// FollowUps returns the verify Work every L2 mode needs (VERIFICATION.md §L2).
func (mode) FollowUps(env *protocol.ResultEnvelope, c modes.FollowUpContext) []modes.WorkSpec {
	return modes.VerifyWork(env, c)
}
