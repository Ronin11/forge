// Package run implements the run mode: one prompt, one repository, current
// context — the routine prompt is the whole task (MODES.md §run).
package run

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

// New returns the run mode.
func New() modes.Mode { return mode{} }

// mode answers the Mode contract with run's constants from MODES.md.
type mode struct{}

func (mode) Name() string     { return "run" }
func (mode) Preamble() string { return preamble }

func (mode) AllowedTools() []string {
	return append(modes.Builtins(),
		"forge_repo_status", "forge_check", "forge_diff_summary",
		"forge_note_progress", "forge_kb_search", "forge_usage")
}

// ResultSchema is the plain envelope: run adds no fields.
func (mode) ResultSchema() json.RawMessage { return schema.MustExtend("") }

func (mode) Level() model.VerificationLevel  { return model.L1 }
func (mode) Checkpoints() []string           { return nil }
func (mode) DefaultClass() model.BudgetClass { return model.ClassNormal }

// DefaultAutonomy is empty: run inherits the project default
// (model.ResolveAutonomy treats empty as unset).
func (mode) DefaultAutonomy() model.Autonomy { return "" }

func (mode) Writes() model.WriteScope { return model.WritesRepo }

// FollowUps spawns nothing: run is L1 and hands off to nobody.
func (mode) FollowUps(*protocol.ResultEnvelope, modes.FollowUpContext) []modes.WorkSpec {
	return nil
}
