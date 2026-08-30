package model

import "fmt"

// ProposalKind is what a proposal changes (DESIGN.md §12). Each kind carries
// its own verification before apply; `code` is never applied by Forge at all
// (constitution 7).
type ProposalKind string

const (
	ProposalRoutine    ProposalKind = "routine"     // new generation of prompt/settings
	ProposalModePrompt ProposalKind = "mode_prompt" // <home>/modes/<mode>.md
	ProposalDoc        ProposalKind = "doc"         // a kb note or repo doc
	ProposalTool       ProposalKind = "tool"        // new script tool under <home>/tools/
	ProposalProcess    ProposalKind = "process"     // schedule / class / autonomy / deps
	ProposalCode       ProposalKind = "code"        // forge/… branch on the Forge repo
)

// ValidProposalKind reports whether k is one of the six kinds.
func ValidProposalKind(k ProposalKind) bool {
	switch k {
	case ProposalRoutine, ProposalModePrompt, ProposalDoc, ProposalTool, ProposalProcess, ProposalCode:
		return true
	}
	return false
}

// ProposalStatus is the funnel state (proposed → approved → applied →
// reverted; rejected is terminal from proposed).
type ProposalStatus string

const (
	ProposalProposed ProposalStatus = "proposed"
	ProposalApproved ProposalStatus = "approved"
	ProposalRejected ProposalStatus = "rejected"
	ProposalApplied  ProposalStatus = "applied"
	ProposalReverted ProposalStatus = "reverted"
)

// ValidProposalStatus reports whether s is a known status.
func ValidProposalStatus(s ProposalStatus) bool {
	switch s {
	case ProposalProposed, ProposalApproved, ProposalRejected, ProposalApplied, ProposalReverted:
		return true
	}
	return false
}

// proposalTransitions is the status machine: decisions from proposed, apply
// after approval, revert only from applied.
var proposalTransitions = map[ProposalStatus][]ProposalStatus{
	ProposalProposed: {ProposalApproved, ProposalRejected},
	ProposalApproved: {ProposalApplied},
	ProposalApplied:  {ProposalReverted},
}

// ProposalTransition validates from → to.
func ProposalTransition(from, to ProposalStatus) error {
	for _, ok := range proposalTransitions[from] {
		if ok == to {
			return nil
		}
	}
	return fmt.Errorf("proposal transition %s → %s is not allowed", from, to)
}
