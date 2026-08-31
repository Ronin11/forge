package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"forge/internal/model"
)

// Proposal is one self-improvement suggestion (DESIGN.md §12): reflection
// proposes, a human decides, Forge applies only what the approval covers.
type Proposal struct {
	ID               string               `json:"id"`
	Source           string               `json:"source"` // retro note id | manual | attempt:<id>
	Kind             model.ProposalKind   `json:"kind"`
	Target           string               `json:"target"`
	Before           json.RawMessage      `json:"before,omitempty"`
	After            json.RawMessage      `json:"after,omitempty"`
	Rationale        string               `json:"rationale"`
	VerificationPlan string               `json:"verification_plan"`
	Status           model.ProposalStatus `json:"status"`
	DecidedBy        string               `json:"decided_by,omitempty"`
	DecidedAt        time.Time            `json:"decided_at,omitempty"`
	AppliedRef       string               `json:"applied_ref,omitempty"`
	OutcomeMetrics   json.RawMessage      `json:"outcome_metrics,omitempty"`
	EvalScore        *float64             `json:"eval_score,omitempty"`
	ExternalRefs     json.RawMessage      `json:"external_refs,omitempty"`
	CreatedAt        time.Time            `json:"created_at"`
	UpdatedAt        time.Time            `json:"updated_at"`
}

// CreateProposal inserts a new proposal in `proposed` status. The constitution
// is not a valid target for any kind (constitution 8): rejected at creation.
func (tx *Tx) CreateProposal(ctx context.Context, p *Proposal) error {
	if !model.ValidProposalKind(p.Kind) {
		return fmt.Errorf("proposal kind %q invalid", p.Kind)
	}
	if p.Target == "" || p.Rationale == "" || p.VerificationPlan == "" {
		return fmt.Errorf("proposal: target, rationale, and verification_plan are required")
	}
	if targetsConstitution(p.Target) {
		return fmt.Errorf("proposal targets the constitution: %w", ErrConflict)
	}
	// A proposal that applies a concrete change must carry it: an empty `after`
	// makes the proposal unapplyable, so approving it later dead-ends on
	// "after is required". Doc proposals are the exception (apply just records
	// them). This is the create-time guard against advisory-only routine/etc.
	// proposals (an early retro filed some before its prompt required `after`).
	switch p.Kind {
	case model.ProposalRoutine, model.ProposalModePrompt, model.ProposalProcess, model.ProposalTool, model.ProposalCode:
		a := strings.TrimSpace(string(p.After))
		if a == "" || a == "null" || a == "{}" {
			return fmt.Errorf("proposal of kind %s requires a concrete 'after' (the change to apply)", p.Kind)
		}
	}
	p.ID = model.NewID()
	p.Status = model.ProposalProposed
	p.CreatedAt, p.UpdatedAt = tx.now, tx.now
	if _, err := tx.Exec(ctx, `INSERT INTO proposals (id, source, kind, target, before, after, rationale, verification_plan, status, external_refs, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		p.ID, p.Source, string(p.Kind), p.Target, jsonRaw(p.Before), jsonRaw(p.After), p.Rationale, p.VerificationPlan, string(p.Status), jsonRaw(p.ExternalRefs), formatTime(tx.now), formatTime(tx.now)); err != nil {
		return fmt.Errorf("insert proposal: %w", err)
	}
	return tx.Journal(ctx, "proposal.created", EntityProposal, p.ID, map[string]any{"kind": p.Kind, "target": p.Target, "source": p.Source})
}

// targetsConstitution guards constitution 8 at the data layer, matching the
// file by suffix so `docs/CONSTITUTION.md`, a kb ref, or a bare name all hit.
func targetsConstitution(target string) bool {
	for _, s := range []string{"CONSTITUTION.md", "constitution"} {
		if target == s {
			return true
		}
	}
	return len(target) >= len("CONSTITUTION.md") && target[len(target)-len("CONSTITUTION.md"):] == "CONSTITUTION.md"
}

// DecideProposal records a human decision: approved or rejected, from
// proposed only.
func (tx *Tx) DecideProposal(ctx context.Context, id string, to model.ProposalStatus, by string) (*Proposal, error) {
	p, err := tx.getProposal(ctx, id)
	if err != nil {
		return nil, err
	}
	if err := model.ProposalTransition(p.Status, to); err != nil {
		return nil, fmt.Errorf("%w: %w", err, ErrConflict)
	}
	p.Status, p.DecidedBy, p.DecidedAt, p.UpdatedAt = to, by, tx.now, tx.now
	if _, err := tx.Exec(ctx, `UPDATE proposals SET status = ?, decided_by = ?, decided_at = ?, updated_at = ? WHERE id = ?`,
		string(to), by, formatTime(tx.now), formatTime(tx.now), id); err != nil {
		return nil, fmt.Errorf("decide proposal %s: %w", id, err)
	}
	if err := tx.Journal(ctx, "proposal.decided", EntityProposal, id, map[string]any{"status": to, "by": by}); err != nil {
		return nil, err
	}
	return p, nil
}

// MarkProposalApplied moves approved → applied and records what the apply
// produced (a generation, a file, a branch).
func (tx *Tx) MarkProposalApplied(ctx context.Context, id, appliedRef string) (*Proposal, error) {
	p, err := tx.getProposal(ctx, id)
	if err != nil {
		return nil, err
	}
	if err := model.ProposalTransition(p.Status, model.ProposalApplied); err != nil {
		return nil, fmt.Errorf("%w: %w", err, ErrConflict)
	}
	p.Status, p.AppliedRef, p.UpdatedAt = model.ProposalApplied, appliedRef, tx.now
	if _, err := tx.Exec(ctx, `UPDATE proposals SET status = ?, applied_ref = ?, updated_at = ? WHERE id = ?`,
		string(p.Status), appliedRef, formatTime(tx.now), id); err != nil {
		return nil, fmt.Errorf("apply proposal %s: %w", id, err)
	}
	if err := tx.Journal(ctx, "proposal.applied", EntityProposal, id, map[string]any{"applied_ref": appliedRef}); err != nil {
		return nil, err
	}
	return p, nil
}

// MarkProposalReverted moves applied → reverted with the A/B numbers that
// triggered it.
func (tx *Tx) MarkProposalReverted(ctx context.Context, id string, outcome json.RawMessage) (*Proposal, error) {
	p, err := tx.getProposal(ctx, id)
	if err != nil {
		return nil, err
	}
	if err := model.ProposalTransition(p.Status, model.ProposalReverted); err != nil {
		return nil, fmt.Errorf("%w: %w", err, ErrConflict)
	}
	p.Status, p.OutcomeMetrics, p.UpdatedAt = model.ProposalReverted, outcome, tx.now
	if _, err := tx.Exec(ctx, `UPDATE proposals SET status = ?, outcome_metrics = ?, updated_at = ? WHERE id = ?`,
		string(p.Status), jsonRaw(outcome), formatTime(tx.now), id); err != nil {
		return nil, fmt.Errorf("revert proposal %s: %w", id, err)
	}
	if err := tx.Journal(ctx, "proposal.reverted", EntityProposal, id, map[string]any{"outcome": json.RawMessage(jsonRawOrNullLiteral(outcome))}); err != nil {
		return nil, err
	}
	return p, nil
}

// jsonRawOrNullLiteral keeps the journal payload valid JSON when outcome is empty.
func jsonRawOrNullLiteral(raw json.RawMessage) string {
	if len(raw) == 0 {
		return "null"
	}
	return string(raw)
}

// GetProposal reads one proposal by full or unique-prefix id.
func (s *Store) GetProposal(ctx context.Context, id string) (*Proposal, error) {
	ps, err := scanProposals(each(s.query(ctx, `SELECT `+proposalColumns+` FROM proposals WHERE id = ? OR id LIKE ? || '%' ORDER BY id LIMIT 2`, id, id)))
	if err != nil {
		return nil, err
	}
	switch len(ps) {
	case 0:
		return nil, fmt.Errorf("proposal %s: %w", id, ErrNotFound)
	case 1:
		return &ps[0], nil
	default:
		return nil, fmt.Errorf("proposal id %s is ambiguous: %w", id, ErrConflict)
	}
}

// getProposal is the in-transaction exact-id read used by state changes.
func (tx *Tx) getProposal(ctx context.Context, id string) (*Proposal, error) {
	ps, err := scanProposals(each(tx.Query(ctx, `SELECT `+proposalColumns+` FROM proposals WHERE id = ?`, id)))
	if err != nil {
		return nil, err
	}
	if len(ps) == 0 {
		return nil, fmt.Errorf("proposal %s: %w", id, ErrNotFound)
	}
	return &ps[0], nil
}

// ListProposals returns proposals newest first, optionally filtered by status.
func (s *Store) ListProposals(ctx context.Context, status model.ProposalStatus) ([]Proposal, error) {
	q, args := `SELECT `+proposalColumns+` FROM proposals ORDER BY created_at DESC, id DESC`, []any{}
	if status != "" {
		q, args = `SELECT `+proposalColumns+` FROM proposals WHERE status = ? ORDER BY created_at DESC, id DESC`, []any{string(status)}
	}
	return scanProposals(each(s.query(ctx, q, args...)))
}

// ProposalFunnel is the reflection funnel (DESIGN.md §12): counts by status.
// Applied counts proposals that reached applied (including later-reverted
// ones), so the funnel never shrinks as proposals move forward.
type ProposalFunnel struct {
	Proposed int `json:"proposed"` // everything ever proposed (= total rows)
	Approved int `json:"approved"` // reached approved (approved+applied+reverted)
	Applied  int `json:"applied"`  // reached applied (applied+reverted)
	Reverted int `json:"reverted"`
	Rejected int `json:"rejected"`
}

// Funnel computes the proposal funnel in one scan.
func (s *Store) Funnel(ctx context.Context) (ProposalFunnel, error) {
	var f ProposalFunnel
	rows, err := s.query(ctx, `SELECT status, count(*) FROM proposals GROUP BY status`)
	if err != nil {
		return f, fmt.Errorf("funnel: %w", err)
	}
	err = each(rows, nil)(func(r *sql.Rows) error {
		var status string
		var n int
		if err := r.Scan(&status, &n); err != nil {
			return err
		}
		f.Proposed += n
		switch model.ProposalStatus(status) {
		case model.ProposalApproved:
			f.Approved += n
		case model.ProposalApplied:
			f.Approved += n
			f.Applied += n
		case model.ProposalReverted:
			f.Approved += n
			f.Applied += n
			f.Reverted += n
		case model.ProposalRejected:
			f.Rejected += n
		}
		return nil
	})
	return f, err
}

const proposalColumns = `id, source, kind, target, before, after, rationale, verification_plan, status, decided_by, decided_at, applied_ref, outcome_metrics, eval_score, external_refs, created_at, updated_at`

func scanProposals(iter func(func(*sql.Rows) error) error) ([]Proposal, error) {
	var out []Proposal
	err := iter(func(rows *sql.Rows) error {
		var p Proposal
		var kind, status string
		var before, after, outcome, refs, decidedBy, decidedAt, appliedRef sql.NullString
		var eval sql.NullFloat64
		var created, updated sql.NullString
		if err := rows.Scan(&p.ID, &p.Source, &kind, &p.Target, &before, &after, &p.Rationale, &p.VerificationPlan, &status, &decidedBy, &decidedAt, &appliedRef, &outcome, &eval, &refs, &created, &updated); err != nil {
			return err
		}
		p.Kind, p.Status = model.ProposalKind(kind), model.ProposalStatus(status)
		p.Before, p.After = rawOrNil(before), rawOrNil(after)
		p.OutcomeMetrics, p.ExternalRefs = rawOrNil(outcome), rawOrNil(refs)
		p.DecidedBy, p.AppliedRef = decidedBy.String, appliedRef.String
		if eval.Valid {
			v := eval.Float64
			p.EvalScore = &v
		}
		var err error
		if p.CreatedAt, err = parseTime(created); err != nil {
			return err
		}
		if p.UpdatedAt, err = parseTime(updated); err != nil {
			return err
		}
		if p.DecidedAt, err = parseTime(decidedAt); err != nil {
			return err
		}
		out = append(out, p)
		return nil
	})
	return out, err
}

func rawOrNil(v sql.NullString) json.RawMessage {
	if !v.Valid || v.String == "" {
		return nil
	}
	return json.RawMessage(v.String)
}
