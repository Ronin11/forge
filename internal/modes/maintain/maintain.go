// Package maintain implements the maintain mode: named maintenance —
// dependency bumps, CI fixes, formatting — that must pass the declared
// checks or be reverted (MODES.md §maintain).
package maintain

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

// extras are maintain's result fields beyond the envelope.
const extras = `{"commits":` + schema.Commits + `}`

// New returns the maintain mode.
func New() modes.Mode { return mode{} }

// mode answers the Mode contract with maintain's constants from MODES.md.
type mode struct{}

func (mode) Name() string     { return "maintain" }
func (mode) Preamble() string { return preamble }

func (mode) AllowedTools() []string {
	return append(modes.Builtins(),
		"forge_check", "forge_repo_status", "forge_diff_summary",
		"forge_note_progress", "forge_kb_search", "forge_usage")
}

func (mode) ResultSchema() json.RawMessage { return schema.MustExtend(extras) }

func (mode) Level() model.VerificationLevel  { return model.L1 }
func (mode) Checkpoints() []string           { return nil }
func (mode) DefaultClass() model.BudgetClass { return model.ClassBacklog }

// DefaultAutonomy is empty: maintain inherits the project default.
func (mode) DefaultAutonomy() model.Autonomy { return "" }

func (mode) Writes() model.WriteScope { return model.WritesRepo }

// FollowUps spawns nothing: maintain is verified by L1 alone.
func (mode) FollowUps(*protocol.ResultEnvelope, modes.FollowUpContext) []modes.WorkSpec {
	return nil
}
