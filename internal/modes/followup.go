package modes

import "forge/internal/model"

// VerifyWork is the one home for the verify Work an L2 mode's FollowUps
// returns (VERIFICATION.md §L2): a fresh-session re-check of the subject
// attempt at its head commit, in the same repository, at the subject's
// priority. The class is interactive only when the subject's was — a backlog
// subject's verification is not itself backlog, because the subject is
// already holding a verifying Target (MODES.md §verify).
func VerifyWork(c FollowUpContext) []WorkSpec {
	class := model.ClassNormal
	if c.Class == model.ClassInteractive {
		class = model.ClassInteractive
	}
	return []WorkSpec{{
		Mode:       "verify",
		Repository: c.Repository,
		Title:      "verify " + model.ShortID(c.AttemptID),
		Class:      class,
		Priority:   c.Priority,
		Autonomy:   model.AutonomyAuto,
		Timeout:    1200,
		MaxTurns:   30,
		VerifyOf: &VerifySubject{
			AttemptID: c.AttemptID,
			Branch:    c.Branch,
			Head:      c.Head,
			UI:        c.UI,
		},
	}}
}
