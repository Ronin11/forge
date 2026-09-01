package store

import (
	"context"
	"database/sql"
	"fmt"
	"time"
)

// PathLease is one live write-set lease row (DESIGN.md §10.2, §20): the globs
// a leased Target holds on its repository. Rows are written at claim and
// deleted by Transition when the Target leaves the states
// model.HoldsWriteSet names.
type PathLease struct {
	TargetID   string
	Repository string
	Globs      []string
	AcquiredAt time.Time
}

// PathLeases returns every lease whose Target still holds its write set,
// inside the claim transaction so the pick and the claim are atomic. The
// state list mirrors model.HoldsWriteSet.
func (tx *Tx) PathLeases(ctx context.Context) ([]PathLease, error) {
	return scanPathLeases(each(tx.Query(ctx, pathLeaseSelect)))
}

// PathLeasesRead is the reader-pool twin, for queue display.
func (s *Store) PathLeasesRead(ctx context.Context) ([]PathLease, error) {
	return scanPathLeases(each(s.query(ctx, pathLeaseSelect)))
}

const pathLeaseSelect = `SELECT p.target_id, p.repository_name, p.globs, p.acquired_at
	FROM path_leases p JOIN targets t ON t.id = p.target_id
	WHERE t.state IN ('claimed','preparing','running','verifying')` // model.HoldsWriteSet

func scanPathLeases(iter func(func(*sql.Rows) error) error) ([]PathLease, error) {
	var out []PathLease
	err := iter(func(rows *sql.Rows) error {
		var l PathLease
		var globs, acquired sql.NullString
		if err := rows.Scan(&l.TargetID, &l.Repository, &globs, &acquired); err != nil {
			return fmt.Errorf("scan path lease: %w", err)
		}
		var err error
		if l.Globs, err = jsonStrings(globs); err != nil {
			return err
		}
		if l.AcquiredAt, err = parseTime(acquired); err != nil {
			return err
		}
		out = append(out, l)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read path leases: %w", err)
	}
	return out, nil
}

// MarkLeaseBlocked journals the FIRST time a pending Target is passed over
// for a path lease, so facts can later derive lease_wait_us (wall clock:
// blocked-at to claimed-at spans claim transactions). Repeats are no-ops —
// the wait starts at the first refusal.
func (tx *Tx) MarkLeaseBlocked(ctx context.Context, targetID, holder string) error {
	var n int
	if err := tx.QueryRow(ctx, `SELECT count(*) FROM journal WHERE entity_type = ? AND entity_id = ? AND kind = 'target.lease_blocked'`, EntityTarget, targetID).Scan(&n); err != nil {
		return fmt.Errorf("check lease_blocked of %s: %w", targetID, err)
	}
	if n > 0 {
		return nil
	}
	return tx.Journal(ctx, "target.lease_blocked", EntityTarget, targetID, map[string]string{"holder": holder})
}

// LeaseBlockedAt returns when a Target was first passed over for a path
// lease, or the zero time.
func (s *Store) LeaseBlockedAt(ctx context.Context, targetID string) (time.Time, error) {
	rows, err := s.JournalForEntity(ctx, EntityTarget, targetID)
	if err != nil {
		return time.Time{}, err
	}
	for _, r := range rows {
		if r.Kind == "target.lease_blocked" {
			return r.Time, nil
		}
	}
	return time.Time{}, nil
}
