// Package audit implements the audit mode: a scheduled read-only sweep for
// security, dependency, dead-code, and doc-drift findings, summarised into a
// kb note (MODES.md §audit).
package audit

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

// extras are audit's result fields beyond the envelope; findings share
// review's shape (MODES.md: "findings[] as review").
const extras = `{` +
	`"findings":` + schema.Findings + `,` +
	`"note_id":{"type":"string"}}`

// New returns the audit mode.
func New() modes.Mode { return mode{} }

// mode answers the Mode contract with audit's constants from MODES.md.
type mode struct{}

func (mode) Name() string     { return "audit" }
func (mode) Preamble() string { return preamble }

func (mode) AllowedTools() []string {
	return []string{
		"Read", "Grep", "Glob", "Bash",
		"forge_kb_new", "forge_repo_status",
		"forge_note_progress", "forge_kb_search", "forge_usage",
	}
}

func (mode) ResultSchema() json.RawMessage { return schema.MustExtend(extras) }

func (mode) Level() model.VerificationLevel  { return model.L0 }
func (mode) Checkpoints() []string           { return nil }
func (mode) DefaultClass() model.BudgetClass { return model.ClassBacklog }
func (mode) DefaultAutonomy() model.Autonomy { return model.AutonomyAuto }
func (mode) Writes() model.WriteScope        { return model.WritesKbOnly }

// FollowUps spawns nothing: audit findings wait for a human or a routine.
func (mode) FollowUps(*protocol.ResultEnvelope, modes.FollowUpContext) []modes.WorkSpec {
	return nil
}
