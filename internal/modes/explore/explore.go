// Package explore implements the explore mode: read-only research that ends
// in one kb note answering the routine's question (MODES.md §explore).
package explore

import (
	_ "embed"
	"encoding/json"

	"forge/internal/core/model"
	"forge/internal/modes"
	"forge/internal/modes/schema"
	"forge/internal/protocol"
)

//go:embed preamble.md
var preamble string

// extras are explore's result fields beyond the envelope.
const extras = `{` +
	`"note_id":{"type":"string"},` +
	`"sources":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string"},"why":{"type":"string"}}}}}`

// New returns the explore mode.
func New() modes.Mode { return mode{} }

// mode answers the Mode contract with explore's constants from MODES.md.
type mode struct{}

func (mode) Name() string     { return "explore" }
func (mode) Preamble() string { return preamble }

func (mode) AllowedTools() []string {
	return []string{
		"Read", "Grep", "Glob", "Bash(git log:*)", "Bash(git show:*)",
		"forge_kb_new", "forge_kb_links", "forge_repo_status",
		"forge_note_progress", "forge_kb_search", "forge_usage",
	}
}

func (mode) ResultSchema() json.RawMessage { return schema.MustExtend(extras) }

func (mode) Level() model.VerificationLevel  { return model.L0 }
func (mode) Checkpoints() []string           { return nil }
func (mode) DefaultClass() model.BudgetClass { return model.ClassBacklog }
func (mode) DefaultAutonomy() model.Autonomy { return model.AutonomyAuto }
func (mode) Writes() model.WriteScope        { return model.WritesKbOnly }

// FollowUps spawns nothing: the note is the product.
func (mode) FollowUps(*protocol.ResultEnvelope, modes.FollowUpContext) []modes.WorkSpec {
	return nil
}
