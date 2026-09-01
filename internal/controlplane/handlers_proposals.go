package controlplane

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"

	"forge/internal/core/model"
	"forge/internal/store"
)

// proposalRoutes serves DESIGN.md §12's decision surface: reflection (or a
// human, via POST) proposes, a human decides, and an approval applies inside
// the decision's transaction so a failed apply never half-decides.
func (s *Server) proposalRoutes(m *http.ServeMux) {
	m.HandleFunc("GET /api/v1/proposals", s.handle(s.listProposals))
	m.HandleFunc("POST /api/v1/proposals", s.handle(s.createProposal))
	m.HandleFunc("GET /api/v1/proposals/{id}", s.handle(s.getProposal))
	m.HandleFunc("POST /api/v1/proposals/{id}/approve", s.handle(s.approveProposal))
	m.HandleFunc("POST /api/v1/proposals/{id}/reject", s.handle(s.rejectProposal))
	m.HandleFunc("POST /api/v1/proposals/{id}/eval", s.handle(s.recordProposalEval))
}

func (s *Server) listProposals(r *http.Request) (int, any, error) {
	status := model.ProposalStatus(r.URL.Query().Get("status"))
	if status != "" && !model.ValidProposalStatus(status) {
		return 0, nil, badRequest("status %q: want proposed, approved, rejected, applied, or reverted", status)
	}
	ps, err := s.store.ListProposals(r.Context(), status)
	if err != nil {
		return 0, nil, err
	}
	if ps == nil {
		ps = []store.Proposal{}
	}
	return http.StatusOK, ps, nil
}

// proposalRequest is POST /api/v1/proposals: a manually filed proposal.
type proposalRequest struct {
	Kind             model.ProposalKind `json:"kind"`
	Target           string             `json:"target"`
	Before           json.RawMessage    `json:"before"`
	After            json.RawMessage    `json:"after"`
	Rationale        string             `json:"rationale"`
	VerificationPlan string             `json:"verification_plan"`
}

// proposalWriteError maps what CreateProposal returns, like routineWriteError:
// the sentinels keep their status (the constitution guard wraps ErrConflict);
// everything else it refuses is validated before the insert, so it is the
// client's.
func proposalWriteError(err error) error {
	if err == nil || errors.Is(err, store.ErrNotFound) || errors.Is(err, store.ErrConflict) {
		return err
	}
	return badRequest("%v", err)
}

func (s *Server) createProposal(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	var req proposalRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	if !model.ValidProposalKind(req.Kind) {
		return 0, nil, badRequest("kind %q: want routine, mode_prompt, doc, tool, process, or code", req.Kind)
	}
	p := &store.Proposal{
		Source: "manual", Kind: req.Kind, Target: req.Target, Before: req.Before, After: req.After,
		Rationale: req.Rationale, VerificationPlan: req.VerificationPlan,
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error { return proposalWriteError(tx.CreateProposal(ctx, p)) }); err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "proposal created", "proposal_id", p.ID, "kind", p.Kind, "target", p.Target, "source", p.Source)
	return http.StatusCreated, p, nil
}

// resolveProposal reads the path {id} — a full id or a unique prefix, so
// `forge proposal approve <id8>` works — into the stored row.
func (s *Server) resolveProposal(r *http.Request) (*store.Proposal, error) {
	id := r.PathValue("id")
	if id == "" {
		return nil, badRequest("proposal id is required")
	}
	return s.store.GetProposal(r.Context(), id)
}

func (s *Server) getProposal(r *http.Request) (int, any, error) {
	p, err := s.resolveProposal(r)
	if err != nil {
		return 0, nil, err
	}
	return http.StatusOK, p, nil
}

func (s *Server) approveProposal(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	resolved, err := s.resolveProposal(r)
	if err != nil {
		return 0, nil, err
	}
	force, err := approveForce(r)
	if err != nil {
		return 0, nil, err
	}
	// The eval gate of DESIGN.md §23: a routine or mode_prompt proposal is a
	// prompt change, and a prompt change without a measured eval score is a
	// guess. The other kinds carry their own verification at apply time. A
	// human force override skips the gate — the A/B auto-revert still guards a
	// routine change — and is journalled below as approved without a score.
	gated := resolved.EvalScore == nil && (resolved.Kind == model.ProposalRoutine || resolved.Kind == model.ProposalModePrompt)
	if gated && !force {
		return 0, nil, fmt.Errorf("no eval score yet — run `forge eval --record-proposal %s` first: %w", model.ShortID(resolved.ID), store.ErrConflict)
	}
	var p *store.Proposal
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		var err error
		if gated && force {
			// Record the bypass in the same transaction as the decision so the
			// audit trail shows this proposal was approved without a score.
			if err := tx.Journal(ctx, "proposal.force_approved", store.EntityProposal, resolved.ID, map[string]any{"proposal_id": resolved.ID, "by": "human"}); err != nil {
				return err
			}
		}
		p, err = tx.DecideProposal(ctx, resolved.ID, model.ProposalApproved, "human")
		if err != nil {
			return err
		}
		// applyProposal runs inside the decision's transaction: a failed apply
		// rolls the approval back too, so the proposal stays proposed and an
		// approved-but-unapplied row never needs repair.
		ref, err := s.applyProposal(ctx, tx, p)
		if err != nil {
			return fmt.Errorf("apply proposal %s: %w: %w", resolved.ID, err, store.ErrConflict)
		}
		if ref != "" {
			p, err = tx.MarkProposalApplied(ctx, p.ID, ref)
			return err
		}
		return nil
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "proposal approved", "proposal_id", p.ID, "kind", p.Kind, "status", p.Status, "applied_ref", p.AppliedRef, "forced", gated && force)
	return http.StatusOK, p, nil
}

// approveForce reads the human override that skips the eval-score gate, from
// the query (?force=true) or a JSON body {"force":true}. The request may carry
// no body — a bare POST is force=false — so an empty body is not an error.
func approveForce(r *http.Request) (bool, error) {
	if r.URL.Query().Get("force") == "true" {
		return true, nil
	}
	if r.ContentLength == 0 {
		return false, nil
	}
	var req struct {
		Force bool `json:"force"`
	}
	if err := decodeJSON(r, &req); err != nil {
		return false, err
	}
	return req.Force, nil
}

// rejectRequest is reject's optional body. The reason is echoed back, not
// stored: the journal row DecideProposal writes is the audit trail of the
// decision, and a rejected proposal carries no free-text column.
type rejectRequest struct {
	Reason string `json:"reason"`
}

// rejectResponse returns the decided proposal with the caller's reason echoed.
type rejectResponse struct {
	Proposal *store.Proposal `json:"proposal"`
	Reason   string          `json:"reason,omitempty"`
}

func (s *Server) rejectProposal(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	resolved, err := s.resolveProposal(r)
	if err != nil {
		return 0, nil, err
	}
	var req rejectRequest
	if r.ContentLength != 0 {
		if err := decodeJSON(r, &req); err != nil {
			return 0, nil, err
		}
	}
	var p *store.Proposal
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		var err error
		p, err = tx.DecideProposal(ctx, resolved.ID, model.ProposalRejected, "human")
		return err
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "proposal rejected", "proposal_id", p.ID, "kind", p.Kind)
	return http.StatusOK, rejectResponse{Proposal: p, Reason: req.Reason}, nil
}

// evalScoreRequest is POST /api/v1/proposals/{id}/eval: `forge eval
// --record-proposal` posting the summary score before an approval.
type evalScoreRequest struct {
	Score *float64 `json:"score"`
}

func (s *Server) recordProposalEval(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	resolved, err := s.resolveProposal(r)
	if err != nil {
		return 0, nil, err
	}
	var req evalScoreRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	if req.Score == nil {
		return 0, nil, badRequest("score is required")
	}
	var p *store.Proposal
	err = s.store.Write(ctx, func(tx *store.Tx) error {
		var werr error
		p, werr = tx.SetProposalEvalScore(ctx, resolved.ID, *req.Score)
		return proposalWriteError(werr)
	})
	if err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "proposal eval score recorded", "proposal_id", p.ID, "score", *req.Score)
	return http.StatusOK, p, nil
}
