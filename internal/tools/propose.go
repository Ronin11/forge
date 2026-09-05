package tools

// forge_propose files a Proposal (DESIGN.md §12): reflection suggests a
// change with the verification it will be judged by, a human decides it, and
// only what the approval covers is ever applied.

import (
	"context"
	"encoding/json"
	"strings"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

type proposeTool struct{}

func (proposeTool) Name() string { return "forge_propose" }
func (proposeTool) Description() string {
	return "File a self-improvement Proposal (kind routine, mode_prompt, doc, tool, process, code, or workflow) for a human to approve; nothing is applied until then."
}
func (proposeTool) Where() string { return WhereDaemon }
func (proposeTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{
		"kind":{"type":"string","enum":["routine","mode_prompt","doc","tool","process","code","workflow"]},
		"prediction":{"type":"object","description":"optional falsifiable forecast: what will hold if this proposal is right, with your confidence — resolved against the A/B net and scored for calibration","properties":{"statement":{"type":"string"},"probability":{"type":"number","minimum":0,"maximum":1}},"required":["statement","probability"],"additionalProperties":false},
		"target":{"type":"string","description":"what the proposal changes: a routine name (a routine whose target is a directive gets its prompt/model/effort updates written to the directive file in the library), a mode, a doc path, a tool name"},
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
		Prediction       *struct {
			Statement   string  `json:"statement"`
			Probability float64 `json:"probability"`
		} `json:"prediction"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if !model.ValidProposalKind(model.ProposalKind(in.Kind)) {
		return nil, BadInput("kind %q: want routine, mode_prompt, doc, tool, process, code, or workflow", in.Kind)
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
	if err := req.Deps.Write(ctx, func(tx *store.Tx) error {
		if err := tx.CreateProposal(ctx, p); err != nil {
			return err
		}
		if in.Prediction == nil {
			return nil
		}
		if in.Prediction.Probability < 0 || in.Prediction.Probability > 1 || in.Prediction.Statement == "" {
			return BadInput("prediction: statement and probability in [0,1] are required")
		}
		// A stated forecast makes the proposal falsifiable: it resolves
		// against the proposal's fate (reverted = failed, survived the
		// horizon applied = held) and scores the source's calibration.
		bucket, _, _ := strings.Cut(source, ":")
		prob := in.Prediction.Probability
		return tx.InsertPrediction(ctx, &store.Prediction{
			Source: bucket, SourceRef: req.AttemptID, ProposalID: p.ID, Subject: in.Target,
			Statement: in.Prediction.Statement, Probability: &prob,
			ResolveBy: req.Deps.Clock().Add(7 * 24 * time.Hour),
		})
	}); err != nil {
		return nil, err
	}
	return respond(map[string]any{"schema_version": SchemaVersion, "id": p.ID, "status": p.Status})
}
