// Package doctor holds every health check `forge doctor` can report — the
// local ones the CLI runs with the daemon down, and the daemon-side ones served
// by GET /api/v1/doctor. One package holds every check (STYLE.md §2) so the
// rule for what is healthy has one home; the CLI and the handler only gather
// inputs and render.
package doctor

// Statuses a Check can report. A warn is degraded but working; only a fail
// makes `forge doctor` exit non-zero.
const (
	StatusOK   = "ok"
	StatusWarn = "warn"
	StatusFail = "fail"
)

// Check is one row of the doctor table: what was checked, how it stands, and —
// for anything not ok — the one command or edit that fixes it.
type Check struct {
	Name   string `json:"name"`
	Status string `json:"status"` // ok | warn | fail
	Detail string `json:"detail"`
	Hint   string `json:"hint,omitempty"`
}

// AnyFailed reports whether any check is a hard failure (warn does not count),
// which is the exit-code rule of `forge doctor`.
func AnyFailed(checks []Check) bool {
	for _, c := range checks {
		if c.Status == StatusFail {
			return true
		}
	}
	return false
}
