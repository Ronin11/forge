// Package integrate implements the integrate mode: given a task branch whose
// rebase onto the integration branch conflicted, resolve ONLY the conflict —
// no new dependencies, no web, tight budget (DESIGN.md §20, MODES.md).
package integrate

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

// extras are integrate's result fields beyond the envelope: which files were
// resolved, and whether anything was left for a human.
const extras = `{` +
	`"resolved":{"type":"array","items":{"type":"string"}},` +
	`"unresolved":{"type":"array","items":{"type":"string"}}}`

// New returns the integrate mode.
func New() modes.Mode { return mode{} }

// mode answers the Mode contract with integrate's constants (DESIGN.md §20).
type mode struct{}

func (mode) Name() string     { return "integrate" }
func (mode) Preamble() string { return preamble }

// AllowedTools keeps the built-ins (it must edit files) plus the repo tools;
// WebFetch and WebSearch are deliberately absent — conflict resolution never
// needs the network.
func (mode) AllowedTools() []string {
	return []string{
		"Bash", "Edit", "Glob", "Grep", "Read", "TodoWrite", "Write",
		"forge_check", "forge_repo_status", "forge_diff_summary",
		"forge_note_progress", "forge_kb_search", "forge_usage",
	}
}

func (mode) ResultSchema() json.RawMessage { return schema.MustExtend(extras, "resolved") }

func (mode) Level() model.VerificationLevel  { return model.L1 }
func (mode) Checkpoints() []string           { return nil }
func (mode) DefaultClass() model.BudgetClass { return model.ClassInteractive }
func (mode) DefaultAutonomy() model.Autonomy { return model.AutonomyAuto }
func (mode) Writes() model.WriteScope        { return model.WritesRepo }

// FollowUps spawns nothing: the merge queue re-runs the rebase itself.
func (mode) FollowUps(*protocol.ResultEnvelope, modes.FollowUpContext) []modes.WorkSpec {
	return nil
}
