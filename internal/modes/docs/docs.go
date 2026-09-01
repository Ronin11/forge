// Package docs implements the docs mode: diff-driven documentation sync,
// restricted to the repository's declared doc paths (MODES.md §docs).
package docs

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

// extras are docs' result fields beyond the envelope.
const extras = `{"docs_updated":{"type":"array","items":{"type":"string"}}}`

// New returns the docs mode.
func New() modes.Mode { return mode{} }

// mode answers the Mode contract with docs' constants from MODES.md.
type mode struct{}

func (mode) Name() string     { return "docs" }
func (mode) Preamble() string { return preamble }

func (mode) AllowedTools() []string {
	return append(modes.Builtins(),
		"forge_diff_summary", "forge_repo_status", "forge_check",
		"forge_note_progress", "forge_kb_search", "forge_usage")
}

func (mode) ResultSchema() json.RawMessage { return schema.MustExtend(extras) }

func (mode) Level() model.VerificationLevel  { return model.L1 }
func (mode) Checkpoints() []string           { return nil }
func (mode) DefaultClass() model.BudgetClass { return model.ClassBacklog }

// DefaultAutonomy is empty: docs inherits the project default.
func (mode) DefaultAutonomy() model.Autonomy { return "" }

func (mode) Writes() model.WriteScope { return model.WritesDocsOnly }

// FollowUps spawns nothing: docs is verified by L1 and its write scope.
func (mode) FollowUps(*protocol.ResultEnvelope, modes.FollowUpContext) []modes.WorkSpec {
	return nil
}
