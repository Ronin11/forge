// Package retro implements the retro mode: a data pack in, a kb retro note
// and proposals out, with no built-in tools at all (MODES.md §retro).
package retro

import (
	_ "embed"
	"encoding/json"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
	"forge/internal/modes"
	"forge/internal/modes/schema"
)

//go:embed preamble.md
var preamble string

// extras are retro's result fields beyond the envelope.
const extras = `{` +
	`"note_id":{"type":"string"},` +
	`"proposals":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["id","kind","target"],"properties":{"id":{"type":"string"},"kind":{"type":"string"},"target":{"type":"string"}}}},` +
	`"hypotheses":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["statement","metric"],"properties":{"statement":{"type":"string"},"metric":{"type":"string"},"expected_delta":{"type":"string"}}}}}`

// New returns the retro mode.
func New() modes.Mode { return mode{} }

// mode answers the Mode contract with retro's constants from MODES.md.
type mode struct{}

func (mode) Name() string     { return "retro" }
func (mode) Preamble() string { return preamble }

// AllowedTools starts with modes.SentinelNoBuiltins: retro runs with no
// Claude built-ins (--tools ""), only the forge_* tools listed here.
func (mode) AllowedTools() []string {
	return []string{
		modes.SentinelNoBuiltins,
		"forge_retro_pack", "forge_stats", "forge_attempt", "forge_events",
		"forge_prompt_version", "forge_usage", "forge_queue",
		"forge_kb_search", "forge_kb_new", "forge_kb_note",
		"forge_kb_backlinks", "forge_kb_links", "forge_propose",
		"forge_note_progress",
	}
}

func (mode) ResultSchema() json.RawMessage { return schema.MustExtend(extras) }

func (mode) Level() model.VerificationLevel  { return model.L0 }
func (mode) Checkpoints() []string           { return nil }
func (mode) DefaultClass() model.BudgetClass { return model.ClassBacklog }
func (mode) DefaultAutonomy() model.Autonomy { return model.AutonomyAuto }
func (mode) Writes() model.WriteScope        { return model.WritesKbOnly }

// FollowUps spawns nothing: proposals go through forge_propose, not Work.
func (mode) FollowUps(*protocol.ResultEnvelope, modes.FollowUpContext) []modes.WorkSpec {
	return nil
}
