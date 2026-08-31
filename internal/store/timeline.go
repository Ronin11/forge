package store

import (
	"context"
	"database/sql"
	"fmt"
	"time"
)

// TimelinePhase is one finished-attempt phase for the dashboard timeline: its
// name (from PhaseNames) and duration in microseconds. Zero-duration phases and
// "total" are dropped before an item carries them.
type TimelinePhase struct {
	Name       string `json:"name"`
	DurationUS int64  `json:"duration_us"`
}

// TimelineItem is one attempt on the dashboard's wall-clock timeline: the work
// it belongs to, its target's state, and — for a finished attempt — the phase
// breakdown. FinishedAt is zero and Phases empty while the attempt is running.
type TimelineItem struct {
	AttemptID  string          `json:"attempt_id"`
	WorkID     string          `json:"work_id"`
	Title      string          `json:"title"`
	Routine    string          `json:"routine"`
	Repository string          `json:"repository"`
	State      string          `json:"state"`
	StartedAt  time.Time       `json:"started_at"`
	FinishedAt time.Time       `json:"finished_at"`
	Phases     []TimelinePhase `json:"phases"`
}

// timelineLimit caps the timeline so a busy home never renders an unbounded
// page; the newest starts win.
const timelineLimit = 300

// timelinePhaseColumns are PhaseNames without the "total" roll-up, in order —
// the phase sub-segments the timeline draws inside a finished bar.
var timelinePhaseColumns = PhaseNames[:len(PhaseNames)-1]

// TimelineItems returns every attempt overlapping the window — still running,
// or finished at or after since — newest start first, capped at timelineLimit.
// A finished attempt carries its non-zero phase durations in PhaseNames order
// (excluding total); a running one has none and a zero FinishedAt.
func (s *Store) TimelineItems(ctx context.Context, since time.Time) ([]TimelineItem, error) {
	const q = `SELECT a.id, t.work_id, w.title, w.routine_name, t.repository_name, t.state,
		COALESCE(a.started_at, a.created_at), a.finished_at,
		f.queue_wait_us, f.fetch_us, f.resolve_base_us, f.worktree_add_us, f.manifest_us, f.agent_us, f.git_inspect_us, f.verify_us, f.cleanup_us
		FROM attempts a
		JOIN targets t ON t.id = a.target_id
		JOIN work w ON w.id = t.work_id
		LEFT JOIN attempt_facts f ON f.attempt_id = a.id
		WHERE a.finished_at IS NULL OR a.finished_at >= ?
		ORDER BY COALESCE(a.started_at, a.created_at) DESC
		LIMIT ?`
	var out []TimelineItem
	err := each(s.query(ctx, q, formatTime(since), timelineLimit))(func(rows *sql.Rows) error {
		var it TimelineItem
		var started, finished sql.NullString
		phases := make([]sql.NullInt64, len(timelinePhaseColumns))
		dest := []any{&it.AttemptID, &it.WorkID, &it.Title, &it.Routine, &it.Repository, &it.State, &started, &finished}
		for i := range phases {
			dest = append(dest, &phases[i])
		}
		if err := rows.Scan(dest...); err != nil {
			return fmt.Errorf("scan timeline item: %w", err)
		}
		var err error
		if it.StartedAt, err = parseTime(started); err != nil {
			return err
		}
		if it.FinishedAt, err = parseTime(finished); err != nil {
			return err
		}
		for i, name := range timelinePhaseColumns {
			if phases[i].Valid && phases[i].Int64 != 0 {
				it.Phases = append(it.Phases, TimelinePhase{Name: name, DurationUS: phases[i].Int64})
			}
		}
		out = append(out, it)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("timeline items since %s: %w", formatTime(since), err)
	}
	return out, nil
}
