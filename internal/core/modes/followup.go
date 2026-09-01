package modes

import (
	"fmt"
	"strings"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
)

// VerifyWork is the one home for the verify Work an L2 mode's FollowUps
// returns (VERIFICATION.md §L2): a fresh-session re-check of the subject
// attempt at its head commit, in the same repository, at the subject's
// priority. The class is interactive only when the subject's was — a backlog
// subject's verification is not itself backlog, because the subject is
// already holding a verifying Target (MODES.md §verify).
//
// The subject's summary and claims are rendered into the Work's prompt here,
// at creation time, from the parsed envelope — the verify session shares no
// context with the subject, so the claims must travel in the prompt
// (VERIFICATION.md: "the prompt renders the claims"). M4 smoke 22 found the
// prompt empty: the verify agent saw no claims and returned inconclusive.
func VerifyWork(env *protocol.ResultEnvelope, c FollowUpContext) []WorkSpec {
	class := model.ClassNormal
	if c.Class == model.ClassInteractive {
		class = model.ClassInteractive
	}
	var p strings.Builder
	fmt.Fprintf(&p, "SUBJECT: attempt %s on {{repo}}, branch %s, head %s. The subject attempt has completed; its branch at that head is checked out in your worktree.\n",
		model.ShortID(c.AttemptID), c.Branch, c.Head)
	if env != nil && env.Summary != "" {
		fmt.Fprintf(&p, "\nSUBJECT'S SUMMARY (a claim, not a fact):\n%s\n", env.Summary)
	}
	if env != nil && len(env.Claims) > 0 {
		p.WriteString("\nCLAIMS TO VERIFY:\n")
		for i, cl := range env.Claims {
			fmt.Fprintf(&p, "%d. %s\n   claimed evidence: %s\n", i+1, cl.Claim, cl.Evidence)
		}
	} else {
		p.WriteString("\nThe subject declared no claims[]; verify its summary against what you observe and say so in your verdict.\n")
	}
	return []WorkSpec{{
		Mode:       "verify",
		Repository: c.Repository,
		Title:      "verify " + model.ShortID(c.AttemptID),
		Prompt:     p.String(),
		Class:      class,
		Priority:   c.Priority,
		Autonomy:   model.AutonomyAuto,
		// 60/2400 rather than 30/1200: real subjects (a full `just check`,
		// Playwright runs) exhausted 30 turns and died on the executor's
		// --max-turns cliff before supervision's soft-budget nudge (~80% of
		// [supervision].soft_turns) could ever fire.
		Timeout:  2400,
		MaxTurns: 60,
		VerifyOf: &VerifySubject{
			AttemptID: c.AttemptID,
			Branch:    c.Branch,
			Head:      c.Head,
			UI:        c.UI,
		},
	}}
}
