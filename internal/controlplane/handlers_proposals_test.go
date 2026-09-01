package controlplane

import (
	"encoding/json"
	"net/http"
	"testing"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

func createProposalVia(h *harness, target string) store.Proposal {
	h.t.Helper()
	var p store.Proposal
	h.call(http.MethodPost, "/api/v1/proposals", map[string]any{
		"kind": "process", "target": target, "after": map[string]any{"priority": 40},
		"rationale": "trim the timeout", "verification_plan": "watch the next 5 runs",
	}, &p, http.StatusCreated)
	return p
}

func TestProposalLifecycle(t *testing.T) {
	h := newHarness(t, "unix")
	p := createProposalVia(h, "routine:inventory")
	if p.Source != "manual" || p.Status != model.ProposalProposed || p.Kind != model.ProposalProcess {
		t.Fatalf("created = %+v", p)
	}

	// Refusals before the row exists: an unknown kind and the constitution.
	if status, msg := h.do(http.MethodPost, "/api/v1/proposals",
		map[string]any{"kind": "vibe", "target": "x", "rationale": "r", "verification_plan": "v"}, nil, ""); status != http.StatusBadRequest {
		t.Errorf("bad kind = %d %s", status, msg)
	}
	if status, msg := h.do(http.MethodPost, "/api/v1/proposals",
		map[string]any{"kind": "doc", "target": "docs/CONSTITUTION.md", "rationale": "r", "verification_plan": "v"}, nil, ""); status != http.StatusConflict {
		t.Errorf("constitution target = %d %s", status, msg)
	}

	// List, filtered and unfiltered; a bogus status is the client's mistake.
	var list []store.Proposal
	h.call(http.MethodGet, "/api/v1/proposals?status=proposed", nil, &list, http.StatusOK)
	if len(list) != 1 || list[0].ID != p.ID {
		t.Errorf("list proposed = %+v", list)
	}
	h.call(http.MethodGet, "/api/v1/proposals?status=applied", nil, &list, http.StatusOK)
	if len(list) != 0 {
		t.Errorf("list applied = %+v", list)
	}
	if status, _ := h.do(http.MethodGet, "/api/v1/proposals?status=bogus", nil, nil, ""); status != http.StatusBadRequest {
		t.Errorf("bogus status filter = %d", status)
	}

	// Prefix lookup on the id routes; an unknown id is 404.
	var got store.Proposal
	h.call(http.MethodGet, "/api/v1/proposals/"+p.ID[:8], nil, &got, http.StatusOK)
	if got.ID != p.ID {
		t.Errorf("prefix get = %s, want %s", got.ID, p.ID)
	}
	if status, _ := h.do(http.MethodGet, "/api/v1/proposals/ffffffff", nil, nil, ""); status != http.StatusNotFound {
		t.Errorf("unknown proposal = %d", status)
	}

	// Reject by prefix with a reason: decided by "human", the reason echoed.
	var rej rejectResponse
	h.call(http.MethodPost, "/api/v1/proposals/"+p.ID[:8]+"/reject", map[string]string{"reason": "not this week"}, &rej, http.StatusOK)
	if rej.Proposal.Status != model.ProposalRejected || rej.Proposal.DecidedBy != "human" || rej.Reason != "not this week" {
		t.Errorf("reject = %+v reason %q", rej.Proposal, rej.Reason)
	}

	// Rejected is terminal: neither decision applies twice.
	if status, _ := h.do(http.MethodPost, "/api/v1/proposals/"+p.ID+"/reject", nil, nil, ""); status != http.StatusConflict {
		t.Errorf("second reject = %d", status)
	}
	if status, _ := h.do(http.MethodPost, "/api/v1/proposals/"+p.ID+"/approve", nil, nil, ""); status != http.StatusConflict {
		t.Errorf("approve after reject = %d", status)
	}
}

// TestProposalApproveAtomic pins the transaction invariant without depending
// on the apply engine's per-kind behaviour: a 200 means the proposal moved to
// approved (or straight to applied, with a ref); a 409 from a refused apply
// means the whole transaction rolled back and the proposal is still exactly
// proposed — never approved-but-unapplied.
func TestProposalApproveAtomic(t *testing.T) {
	h := newHarness(t, "unix")
	p := createProposalVia(h, "routine:inventory")
	status, raw := h.do(http.MethodPost, "/api/v1/proposals/"+p.ID+"/approve", nil, nil, "")
	switch status {
	case http.StatusOK:
		var out store.Proposal
		if err := json.Unmarshal(raw, &out); err != nil {
			t.Fatalf("decode %s: %v", raw, err)
		}
		if out.DecidedBy != "human" {
			t.Errorf("decided_by = %q", out.DecidedBy)
		}
		switch out.Status {
		case model.ProposalApproved:
			if out.AppliedRef != "" {
				t.Errorf("approved with an applied_ref %q", out.AppliedRef)
			}
		case model.ProposalApplied:
			if out.AppliedRef == "" {
				t.Error("applied without an applied_ref")
			}
		default:
			t.Errorf("status after approve = %s", out.Status)
		}
	default:
		// Any refused apply (404 unknown routine, 409 conflict, 400 bad after)
		// must roll the whole approval back — never approved-but-unapplied.
		var out store.Proposal
		h.call(http.MethodGet, "/api/v1/proposals/"+p.ID, nil, &out, http.StatusOK)
		if out.Status != model.ProposalProposed || out.DecidedBy != "" {
			t.Errorf("after refused apply (%d) = %s decided by %q, want proposed and undecided", status, out.Status, out.DecidedBy)
		}
	}
}
