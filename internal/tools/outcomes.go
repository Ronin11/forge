package tools

// forge_work_outcomes: the supervise mode's eyes. A continuation Work's
// prompt is frozen before its batch runs, so what the children actually did
// arrives here at call time: per-work state, result summary, cost, and
// turns for the caller's plan batch (default) or any work in the caller's
// own tree.

import (
	"context"
	"encoding/json"
	"fmt"
)

type workOutcomesTool struct{}

func (workOutcomesTool) Name() string { return "forge_work_outcomes" }
func (workOutcomesTool) Description() string {
	return "Outcomes of the tasks in your plan batch (or any work in your tree): state, result summary, cost, and turns per work. Supervise-mode callers use this FIRST — your prompt was written before the batch ran; this is what actually happened."
}
func (workOutcomesTool) Where() string { return WhereDaemon }
func (workOutcomesTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{
		"work_id":{"type":"string","description":"scope root; default: the batch you supervise (your work's parent)"}
	},"additionalProperties":false}`)
}

func (workOutcomesTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	if req.Deps.WorkOutcomes == nil {
		return nil, fmt.Errorf("work outcomes are unavailable in this process")
	}
	var in struct {
		WorkID string `json:"work_id"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	out, err := req.Deps.WorkOutcomes(ctx, req.Attempt, in.WorkID)
	if err != nil {
		return nil, err
	}
	return respond(map[string]any{"schema_version": SchemaVersion, "outcomes": out})
}
