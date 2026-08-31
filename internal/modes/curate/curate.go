// Package curate implements the curate mode: a scheduled (monthly, backlog)
// consolidation of superseded retro notes and stale hypotheses into one
// summary kb note with supersedes links (MODES.md §curate, DESIGN.md §22).
package curate

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

// extras are curate's result fields beyond the envelope: the summary note and
// the notes it supersedes.
const extras = `{"note_id":{"type":"string"},"superseded":{"type":"array","items":{"type":"string"}}}`

// New returns the curate mode.
func New() modes.Mode { return mode{} }

// mode answers the Mode contract with curate's constants from MODES.md.
type mode struct{}

func (mode) Name() string     { return "curate" }
func (mode) Preamble() string { return preamble }

// AllowedTools is kb-only: no built-ins and no filesystem — curation happens
// entirely through the kb tools.
func (mode) AllowedTools() []string {
	return []string{
		modes.SentinelNoBuiltins,
		"forge_kb_search", "forge_kb_note", "forge_kb_new",
		"forge_kb_backlinks", "forge_kb_links",
		"forge_note_progress", "forge_usage",
	}
}

func (mode) ResultSchema() json.RawMessage { return schema.MustExtend(extras) }

func (mode) Level() model.VerificationLevel  { return model.L0 }
func (mode) Checkpoints() []string           { return nil }
func (mode) DefaultClass() model.BudgetClass { return model.ClassBacklog }
func (mode) DefaultAutonomy() model.Autonomy { return model.AutonomyAuto }
func (mode) Writes() model.WriteScope        { return model.WritesKbOnly }

// FollowUps spawns nothing: a curate note waits for humans and future runs.
func (mode) FollowUps(*protocol.ResultEnvelope, modes.FollowUpContext) []modes.WorkSpec {
	return nil
}
