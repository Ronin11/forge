// Package review implements the review mode: a PR or diff becomes ranked,
// structured findings and a verdict; it never writes (MODES.md §review).
package review

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

// extras are review's result fields beyond the envelope; both are required —
// a review without findings and a verdict said nothing.
const extras = `{` +
	`"findings":` + schema.Findings + `,` +
	`"verdict":{"type":"string","enum":["approve","request_changes","comment"]}}`

// New returns the review mode.
func New() modes.Mode { return mode{} }

// mode answers the Mode contract with review's constants from MODES.md.
type mode struct{}

func (mode) Name() string     { return "review" }
func (mode) Preamble() string { return preamble }

func (mode) AllowedTools() []string {
	return []string{
		"Read", "Grep", "Glob",
		"Bash(git diff:*)", "Bash(git log:*)", "Bash(git show:*)",
		"forge_diff_summary", "forge_repo_status",
		"forge_note_progress", "forge_kb_search", "forge_usage",
	}
}

func (mode) ResultSchema() json.RawMessage {
	return schema.MustExtend(extras, "findings", "verdict")
}

func (mode) Level() model.VerificationLevel  { return model.L0 }
func (mode) Checkpoints() []string           { return nil }
func (mode) DefaultClass() model.BudgetClass { return model.ClassNormal }
func (mode) DefaultAutonomy() model.Autonomy { return model.AutonomyAuto }
func (mode) Writes() model.WriteScope        { return model.WritesNone }

// FollowUps spawns nothing: a review ends with its verdict.
func (mode) FollowUps(*protocol.ResultEnvelope, modes.FollowUpContext) []modes.WorkSpec {
	return nil
}
