package controlplane

import (
	"encoding/json"
	"net/http"
	"strings"
	"testing"

	"forge/internal/model"
	"forge/internal/store"
)

// createKindProposal files a proposal of an arbitrary kind through the API.
func createKindProposal(h *harness, kind, target string, after map[string]any) store.Proposal {
	h.t.Helper()
	var p store.Proposal
	h.call(http.MethodPost, "/api/v1/proposals", map[string]any{
		"kind": kind, "target": target, "after": after,
		"rationale": "measured on the golden set", "verification_plan": "forge eval",
	}, &p, http.StatusCreated)
	return p
}

// TestApproveRequiresEvalScore pins DESIGN.md §23's gate: routine and
// mode_prompt proposals cannot be approved until forge eval recorded a score;
// the other kinds stay ungated (TestProposalLifecycle covers process).
func TestApproveRequiresEvalScore(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")

	for _, kind := range []string{"routine", "mode_prompt"} {
		target := "routine:inventory"
		if kind == "mode_prompt" {
			target = "mode:run"
		}
		p := createKindProposal(h, kind, target, map[string]any{"prompt": "new prompt"})
		status, body := h.do(http.MethodPost, "/api/v1/proposals/"+p.ID+"/approve", nil, nil, "")
		if status != http.StatusConflict || !strings.Contains(string(body), "no eval score") {
			t.Errorf("%s approve without score = %d %s", kind, status, body)
		}
		var got store.Proposal
		h.call(http.MethodGet, "/api/v1/proposals/"+p.ID, nil, &got, http.StatusOK)
		if got.Status != model.ProposalProposed {
			t.Errorf("%s proposal after refused approve = %s", kind, got.Status)
		}
	}
}

func TestRecordEvalScoreThenApprove(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.createRoutine("inventory")
	p := createKindProposal(h, "routine", "routine:inventory", map[string]any{"prompt": "measured prompt"})

	var scored store.Proposal
	h.call(http.MethodPost, "/api/v1/proposals/"+p.ID+"/eval", map[string]float64{"score": 0.8}, &scored, http.StatusOK)
	if scored.EvalScore == nil || *scored.EvalScore != 0.8 {
		t.Fatalf("scored = %+v", scored.EvalScore)
	}

	// With the score recorded, the gate opens; the apply itself follows
	// TestProposalApproveAtomic's contract (200 applied, or 409 rolled back —
	// but never the eval-score refusal).
	status, body := h.do(http.MethodPost, "/api/v1/proposals/"+p.ID+"/approve", nil, nil, "")
	switch status {
	case http.StatusOK:
		var out store.Proposal
		if err := json.Unmarshal(body, &out); err != nil {
			t.Fatalf("decode %s: %v", body, err)
		}
		if out.EvalScore == nil || *out.EvalScore != 0.8 {
			t.Errorf("approved proposal lost its score: %+v", out.EvalScore)
		}
	case http.StatusConflict:
		if strings.Contains(string(body), "no eval score") {
			t.Errorf("gate still closed after recording: %s", body)
		}
	default:
		t.Fatalf("approve after scoring = %d %s", status, body)
	}
}

func TestRecordEvalScoreRefusals(t *testing.T) {
	h := newHarness(t, transportUnix)
	p := createProposalVia(h, "routine:inventory") // kind process

	if status, _ := h.do(http.MethodPost, "/api/v1/proposals/"+p.ID+"/eval", map[string]any{}, nil, ""); status != http.StatusBadRequest {
		t.Errorf("missing score = %d", status)
	}
	if status, _ := h.do(http.MethodPost, "/api/v1/proposals/"+p.ID+"/eval", map[string]float64{"score": 1.5}, nil, ""); status != http.StatusBadRequest {
		t.Errorf("score 1.5 = %d", status)
	}
	if status, _ := h.do(http.MethodPost, "/api/v1/proposals/ffffffff/eval", map[string]float64{"score": 0.5}, nil, ""); status != http.StatusNotFound {
		t.Errorf("unknown proposal = %d", status)
	}
	// A decided proposal no longer takes a score.
	h.call(http.MethodPost, "/api/v1/proposals/"+p.ID+"/reject", nil, nil, http.StatusOK)
	if status, _ := h.do(http.MethodPost, "/api/v1/proposals/"+p.ID+"/eval", map[string]float64{"score": 0.5}, nil, ""); status != http.StatusConflict {
		t.Errorf("score on rejected = %d", status)
	}
}
