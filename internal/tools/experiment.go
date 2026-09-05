package tools

// forge_experiment opens a live multivariant experiment — the "uncertain →
// trial it" move. When you cannot tell from the evidence whether a prompt
// change helps, do not edit and hope: put the change up as an arm against
// control on real production traffic and let verified outcomes decide.
// Promotion is guarded (posterior confidence, drift aborts, the A/B revert
// net), so an experiment is strictly safer than a direct edit.

import (
	"context"
	"encoding/json"
)

// ExperimentVariant is one proposed arm body (whole fragment file content).
type ExperimentVariant struct {
	Title   string `json:"title"`
	Content string `json:"content"`
}

// ExperimentInput is Deps.OpenExperiment's input.
type ExperimentInput struct {
	Subject  string              `json:"subject"`
	Goal     string              `json:"goal"`
	MinRuns  int                 `json:"min_runs"`
	Variants []ExperimentVariant `json:"variants"`
}

type experimentTool struct{}

func (experimentTool) Name() string { return "forge_experiment" }
func (experimentTool) Description() string {
	return "Open a live experiment on a directive or persona: your variant content (or optimizer-generated arms) runs against control on real production work, and verified outcomes decide. Use this INSTEAD of editing when the evidence is uncertain (wide intervals, small n) or the change is a hypothesis. One live experiment per subject; never edit a subject that is under a live experiment — that aborts the trial."
}
func (experimentTool) Where() string { return WhereDaemon }
func (experimentTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{
		"subject":{"type":"string","description":"directive:<name> or persona:<name>"},
		"goal":{"type":"string","description":"what the subject should do better, and how success shows in verified outcomes"},
		"min_runs":{"type":"integer","minimum":1,"maximum":20,"description":"per-arm decision window; default from config"},
		"variants":{"type":"array","maxItems":2,"items":{"type":"object","properties":{"title":{"type":"string"},"content":{"type":"string","description":"complete replacement file content, frontmatter included"}},"required":["content"],"additionalProperties":false},"description":"your proposed arm bodies; omit to have the optimizer generate candidates"}
	},"required":["subject","goal"],"additionalProperties":false}`)
}

func (experimentTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	if req.Deps.OpenExperiment == nil {
		return nil, BadInput("experiments are not available in this process")
	}
	var in ExperimentInput
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	att := Attempt{ID: req.AttemptID}
	id, err := req.Deps.OpenExperiment(ctx, att, in)
	if err != nil {
		return nil, err
	}
	return respond(map[string]any{"schema_version": SchemaVersion, "id": id, "status": "live_setup", "note": "arms validate asynchronously; the experiment decides itself from verified outcomes"})
}
