package tools

// The control tools: forge_ask records a Question without blocking (MODES.md:
// the answer arrives in a resumed session; an open Question at process exit is
// what moves the Target to waiting_human), and forge_note_progress leaves a
// lifecycle breadcrumb on the attempt's timeline.

import (
	"context"
	"encoding/json"
	"fmt"

	"forge/internal/protocol"
	"forge/internal/store"
)

type askTool struct{}

func (askTool) Name() string { return "forge_ask" }
func (askTool) Description() string {
	return "Record a question for a human without blocking: end the turn after calling; the answer arrives in a resumed session. Allowed only at autonomy ask or checkpoint."
}
func (askTool) Where() string { return WhereDaemon }
func (askTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{
		"question":{"type":"string"},
		"options":{"type":"array","items":{"type":"string"}},
		"context":{"type":"object","description":"anything the answerer should see; an actions array of {label, url} (a UI path to open, e.g. /kb/<id>) or {label, trigger} (registered triggers: notify_test sends a test desktop toast) renders as buttons on the Human queue card"},
		"checkpoint":{"type":"string","description":"the declared checkpoint this question belongs to"}
	},"required":["question"],"additionalProperties":false}`)
}

func (askTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	var in struct {
		Question   string          `json:"question"`
		Options    []string        `json:"options"`
		Context    json.RawMessage `json:"context"`
		Checkpoint string          `json:"checkpoint"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if in.Question == "" {
		return nil, BadInput("question is required")
	}
	if !req.Attempt.Autonomy.AllowsQuestions() {
		return nil, BadInput("autonomy %s does not allow questions", req.Attempt.Autonomy)
	}
	var q *store.Question
	err := req.Deps.Write(ctx, func(tx *store.Tx) error {
		a, err := tx.GetAttempt(ctx, req.AttemptID)
		if err != nil {
			return err
		}
		// CreateQuestion records the Question only; the Target stays where it
		// is (running). The transition to waiting_human happens when the
		// worker completes with an open Question (DESIGN.md §4.1).
		q, err = tx.CreateQuestion(ctx, a, protocol.QuestionRequest{Text: in.Question, Options: in.Options, Context: in.Context, Checkpoint: in.Checkpoint})
		return err
	})
	if err != nil {
		return nil, err
	}
	return respond(map[string]any{
		"schema_version": SchemaVersion,
		"question_id":    q.ID,
		"instruction":    "recorded; end your turn now — the answer arrives in a resumed session",
	})
}

type noteProgressTool struct{}

func (noteProgressTool) Name() string { return "forge_note_progress" }
func (noteProgressTool) Description() string {
	return "Record a short progress message as a lifecycle event on the attempt's timeline."
}
func (noteProgressTool) Where() string { return WhereDaemon }
func (noteProgressTool) InputSchema() json.RawMessage {
	return json.RawMessage(`{"type":"object","properties":{
		"message":{"type":"string"},
		"checkpoint":{"type":"string","description":"the declared checkpoint just reached"}
	},"required":["message"],"additionalProperties":false}`)
}

func (noteProgressTool) Call(ctx context.Context, req Request) (json.RawMessage, error) {
	var in struct {
		Message    string `json:"message"`
		Checkpoint string `json:"checkpoint"`
	}
	if err := decodeInput(req.Input, &in); err != nil {
		return nil, err
	}
	if in.Message == "" {
		return nil, BadInput("message is required")
	}
	err := req.Deps.Write(ctx, func(tx *store.Tx) error {
		if _, err := tx.GetAttempt(ctx, req.AttemptID); err != nil {
			return err
		}
		seq, err := tx.NextControlSeq(ctx, req.AttemptID)
		if err != nil {
			return err
		}
		var attrs json.RawMessage
		if in.Checkpoint != "" {
			b, err := json.Marshal(map[string]string{"checkpoint": in.Checkpoint})
			if err != nil {
				return fmt.Errorf("encode progress attrs: %w", err)
			}
			attrs = b
		}
		ev := protocol.Event{Seq: seq, Time: req.Deps.Clock().UTC(), Kind: protocol.KindLifecycle, Name: "progress", Message: in.Message, Attrs: attrs}
		if _, err := tx.InsertEvents(ctx, req.AttemptID, protocol.SourceControl, []protocol.Event{ev}); err != nil {
			return err
		}
		// Refresh the live progress tally so the note (and its checkpoint) surfaces
		// on the running attempt's task view.
		if err := tx.RecomputeAttemptProgress(ctx, req.AttemptID); err != nil {
			return err
		}
		return tx.Journal(ctx, "attempt.progress", store.EntityAttempt, req.AttemptID, map[string]any{"seq": seq, "checkpoint": in.Checkpoint})
	})
	if err != nil {
		return nil, err
	}
	return respond(map[string]any{"schema_version": SchemaVersion, "ok": true})
}
