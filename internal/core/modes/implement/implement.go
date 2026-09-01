// Package implement implements the implement mode: an issue becomes a branch
// of small commits with verification evidence (MODES.md §implement).
package implement

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

// extras are implement's result fields beyond the envelope.
const extras = `{` +
	`"commits":` + schema.Commits + `,` +
	`"tests_added":{"type":"array","items":{"type":"string"}}}`

// New returns the implement mode.
func New() modes.Mode { return mode{} }

// mode answers the Mode contract with implement's constants from MODES.md.
type mode struct{}

func (mode) Name() string     { return "implement" }
func (mode) Preamble() string { return preamble }

func (mode) AllowedTools() []string {
	return append(modes.Builtins(),
		"forge_check", "forge_repo_status", "forge_diff_summary",
		"forge_note_progress", "forge_kb_search", "forge_usage")
}

func (mode) ResultSchema() json.RawMessage { return schema.MustExtend(extras) }

func (mode) Level() model.VerificationLevel  { return model.L2 }
func (mode) Checkpoints() []string           { return []string{"before_report"} }
func (mode) DefaultClass() model.BudgetClass { return model.ClassNormal }

// DefaultAutonomy is empty: implement inherits the project default.
func (mode) DefaultAutonomy() model.Autonomy { return "" }

func (mode) Writes() model.WriteScope { return model.WritesRepo }

// FollowUps returns the verify Work every L2 mode needs (VERIFICATION.md §L2).
func (mode) FollowUps(env *protocol.ResultEnvelope, c modes.FollowUpContext) []modes.WorkSpec {
	return modes.VerifyWork(env, c)
}
