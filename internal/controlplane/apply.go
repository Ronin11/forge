package controlplane

import (
	"context"

	"forge/internal/store"
)

// applyProposal applies an approved proposal per its kind (DESIGN.md §12) and
// returns the applied_ref ("" means nothing was applied and the proposal
// stays approved). Stub: the M5 apply engine replaces this.
func (s *Server) applyProposal(ctx context.Context, tx *store.Tx, p *store.Proposal) (string, error) {
	_ = ctx
	_ = tx
	_ = p
	return "", nil
}
