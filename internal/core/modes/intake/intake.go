// Package intake implements the intake mode: a bug report or feature request
// becomes an issue with acceptance criteria and, optionally, a handoff to
// implement (MODES.md §intake).
package intake

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

// extras are intake's result fields beyond the envelope; handoff.requested
// is what FollowUps reads to create the implement Work.
const extras = `{` +
	`"issue":{"type":"object","additionalProperties":false,"required":["title","body"],"properties":{"title":{"type":"string"},"body":{"type":"string"},"acceptance_criteria":{"type":"array","items":{"type":"string"}}}},` +
	`"reproduced":{"type":["boolean","null"]},` +
	`"handoff":{"type":"object","additionalProperties":false,"required":["requested"],"properties":{"requested":{"type":"boolean"},"mode":{"type":"string","enum":["implement"]}}},` +
	`"note_id":{"type":"string"}}`

// New returns the intake mode.
func New() modes.Mode { return mode{} }

// mode answers the Mode contract with intake's constants from MODES.md.
type mode struct{}

func (mode) Name() string     { return "intake" }
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
func (mode) Checkpoints() []string           { return []string{"before_handoff"} }
func (mode) DefaultClass() model.BudgetClass { return model.ClassNormal }
func (mode) DefaultAutonomy() model.Autonomy { return model.AutonomyCheckpoint }
func (mode) Writes() model.WriteScope        { return model.WritesKbOnly }

// FollowUps creates the implement Work when the result asked for a handoff
// (MODES.md §intake): prompt = the issue body, same class and priority,
// autonomy left to the resolver. The daemon populates env.Extra from the raw
// result before calling; a nil or partial Extra means no handoff.
func (mode) FollowUps(env *protocol.ResultEnvelope, c modes.FollowUpContext) []modes.WorkSpec {
	if env == nil || env.Extra == nil {
		return nil
	}
	raw, ok := env.Extra["handoff"]
	if !ok {
		return nil
	}
	var handoff struct {
		Requested bool   `json:"requested"`
		Mode      string `json:"mode"`
	}
	if err := json.Unmarshal(raw, &handoff); err != nil || !handoff.Requested {
		return nil
	}
	rawIssue, ok := env.Extra["issue"]
	if !ok {
		return nil
	}
	var issue struct {
		Title string `json:"title"`
		Body  string `json:"body"`
	}
	if err := json.Unmarshal(rawIssue, &issue); err != nil || issue.Body == "" {
		return nil
	}
	return []modes.WorkSpec{{
		Mode:       "implement",
		Repository: c.Repository,
		Title:      issue.Title,
		Prompt:     issue.Body,
		Class:      c.Class,
		Priority:   c.Priority,
	}}
}
