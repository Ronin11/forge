package tools

// forge_propose files a Proposal (DESIGN.md §12): reflection suggests a
// change with the verification it will be judged by, a human decides it, and
// only what the approval covers is ever applied.

import (
	"context"
	"encoding/json"

	"forge/internal/core/model"
	"forge/internal/store"
)

type proposeTool struct{}

func (proposeTool) Name() string { return "forge_propose" }
func (proposeTool) Description() string {
	return "File a self-improvement Proposal (kind routine, mode_prompt, doc, tool, process, or code) for a human to approve; nothing is applied until then."
}
func (proposeTool) Where() string { return WhereDaemon }
func (proposeTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{
		"kind":{"type":"string","enum":["routine","mode_prompt","doc","tool","process","code"]},
		"target":{"type":"string","description":"what the proposal changes: a routine name, a mode, a doc path, a tool name"},
		"before":{"description":"the current value, when it helps the reviewer"},
		"after":{"description":"the proposed value"},
		"rationale":{"type":"string","description":"why, grounded in the retro data"},
		"verification_plan":{"type":"string","description":"how the change will be judged after apply"},
		"source_note":{"type":"string","description":"the kb note this came from; default attempt:<caller>"}
	},"required":["kind","target","rationale","verification_plan"],"additionalProperties":false}`)
}

func (proposeTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	var in struct {
		Kind             string          `json:"kind"`
		Target           string          `json:"target"`
		Before           json.RawMessage `json:"before"`
		After            json.RawMessage `json:"after"`
		Rationale        string          `json:"rationale"`
		VerificationPlan string          `json:"verification_plan"`
		SourceNote       string          `json:"source_note"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if !model.ValidProposalKind(model.ProposalKind(in.Kind)) {
		return nil, BadInput("kind %q: want routine, mode_prompt, doc, tool, process, or code", in.Kind)
	}
	if in.Target == "" || in.Rationale == "" || in.VerificationPlan == "" {
		return nil, BadInput("target, rationale, and verification_plan are required")
	}
	source := in.SourceNote
	if source == "" {
		source = "attempt:" + req.AttemptID
	}
	p := &store.Proposal{
		Source: source, Kind: model.ProposalKind(in.Kind), Target: in.Target,
		Before: in.Before, After: in.After,
		Rationale: in.Rationale, VerificationPlan: in.VerificationPlan,
	}
	// CreateProposal re-validates and enforces constitution 8 (a proposal may
	// never target the constitution); that refusal wraps store.ErrConflict and
	// keeps its status at the HTTP layer.
	if err := req.Deps.Write(ctx, func(tx *store.Tx) error { return tx.CreateProposal(ctx, p) }); err != nil {
		return nil, err
	}
	return respond(map[string]any{"schema_version": SchemaVersion, "id": p.ID, "status": p.Status})
}
