package store

import (
	"fmt"
	"time"

	"github.com/robfig/cron/v3"
)

// Cron schedules. Parsing lives here — validation at save (a bad schedule is
// a 400, not a silently-never-firing row) and next-due math for the
// scheduler loop. Standard 5-field expressions plus @daily-style descriptors
// (robfig's ParseStandard); the daemon's own loop is the runner, no cron
// runtime is embedded.

// ValidateSchedule rejects a cron expression that does not parse; "" is fine
// (no schedule).
func ValidateSchedule(schedule string) error {
	if schedule == "" {
		return nil
	}
	if _, err := cron.ParseStandard(schedule); err != nil {
		return fmt.Errorf("schedule %q: %v", schedule, err)
	}
	return nil
}

// NextOccurrence is the first firing strictly after the given time.
func NextOccurrence(schedule string, after time.Time) (time.Time, error) {
	sched, err := cron.ParseStandard(schedule)
	if err != nil {
		return time.Time{}, fmt.Errorf("schedule %q: %v", schedule, err)
	}
	return sched.Next(after), nil
}
