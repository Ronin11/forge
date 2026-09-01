// Package verify implements the verify mode: an independent, fresh-session
// re-check of another attempt's claims, producing a verdict that decides the
// subject's terminal state (MODES.md §verify, VERIFICATION.md §L2).
package verify

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

// extras are verify's result fields beyond the envelope; verdict is required
// because a verification without one decided nothing.
const extras = `{` +
	`"verdict":{"type":"string","enum":["pass","fail","inconclusive"]},` +
	`"claims_checked":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["claim","result"],"properties":{"claim":{"type":"string"},"result":{"type":"string","enum":["confirmed","refuted","unverifiable"]},"evidence":{"type":"string"},"artifact":{"type":"string"}}}}}`

// New returns the verify mode.
func New() modes.Mode { return mode{} }

// mode answers the Mode contract with verify's constants from MODES.md.
type mode struct{}

func (mode) Name() string     { return "verify" }
func (mode) Preamble() string { return preamble }

// AllowedTools has no Edit or Write: artifacts are written by Bash to the
// artifacts directory, outside the worktree (MODES.md §verify).
func (mode) AllowedTools() []string {
	return []string{
		"Read", "Grep", "Glob", "Bash",
		"forge_check", "forge_attempt", "forge_repo_status",
		"forge_note_progress", "forge_kb_search", "forge_usage",
	}
}

func (mode) ResultSchema() json.RawMessage {
	return schema.MustExtend(extras, "verdict")
}

// Level is L0: this mode is itself the verification of its subject; Forge
// only asserts it wrote nothing.
func (mode) Level() model.VerificationLevel { return model.L0 }

func (mode) Checkpoints() []string { return nil }

// DefaultClass is normal; the daemon raises it to interactive when the
// subject's Work was interactive (modes.VerifyWork sets the class on the
// follow-up spec).
func (mode) DefaultClass() model.BudgetClass { return model.ClassNormal }

func (mode) DefaultAutonomy() model.Autonomy { return model.AutonomyAuto }
func (mode) Writes() model.WriteScope        { return model.WritesNone }

// FollowUps spawns nothing: the verdict acts on the subject, not the queue.
func (mode) FollowUps(*protocol.ResultEnvelope, modes.FollowUpContext) []modes.WorkSpec {
	return nil
}
