package store

import (
	"context"
	"database/sql"
	"fmt"
	"time"
)

// RateLimitSample is one observation of a subscription window.
type RateLimitSample struct {
	Time          time.Time `json:"ts"`
	Window        string    `json:"window"`
	Utilization   float64   `json:"utilization"`
	ResetsAt      time.Time `json:"resets_at"`
	SourceAttempt string    `json:"source_attempt,omitempty"`
}

// InsertSamples stores rate-limit observations (duplicates ignored).
func (tx *Tx) InsertSamples(ctx context.Context, samples []RateLimitSample) error {
	for _, s := range samples {
		if _, err := tx.Exec(ctx, `INSERT OR IGNORE INTO rate_limit_samples (ts, window, utilization, resets_at, source_attempt) VALUES (?, ?, ?, ?, ?)`,
			formatTime(s.Time), s.Window, s.Utilization, formatTime(s.ResetsAt), s.SourceAttempt); err != nil {
			return fmt.Errorf("insert sample: %w", err)
		}
	}
	return nil
}

// SamplesSince returns samples of one window since a time, oldest first.
func (s *Store) SamplesSince(ctx context.Context, window string, since time.Time) ([]RateLimitSample, error) {
	var out []RateLimitSample
	err := each(s.query(ctx, `SELECT ts, window, utilization, resets_at, source_attempt FROM rate_limit_samples WHERE window = ? AND ts >= ? ORDER BY ts`, window, formatTime(since)))(func(rows *sql.Rows) error {
		var sm RateLimitSample
		var ts, resets string
		var src string
		if err := rows.Scan(&ts, &sm.Window, &sm.Utilization, &resets, &src); err != nil {
			return fmt.Errorf("scan sample: %w", err)
		}
		var err error
		if sm.Time, err = parseTime(sql.NullString{String: ts, Valid: true}); err != nil {
			return err
		}
		if sm.ResetsAt, err = parseTime(sql.NullString{String: resets, Valid: true}); err != nil {
			return err
		}
		sm.SourceAttempt = src
		out = append(out, sm)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read samples: %w", err)
	}
	return out, nil
}

// LatestSample returns the newest sample of a window, or nil.
func (s *Store) LatestSample(ctx context.Context, window string) (*RateLimitSample, error) {
	samples, err := scanOneSample(s.queryRow(ctx, `SELECT ts, window, utilization, resets_at, source_attempt FROM rate_limit_samples WHERE window = ? ORDER BY ts DESC LIMIT 1`, window))
	return samples, err
}

func scanOneSample(row *sql.Row) (*RateLimitSample, error) {
	var sm RateLimitSample
	var ts, resets string
	var src string
	err := row.Scan(&ts, &sm.Window, &sm.Utilization, &resets, &src)
	if isNoRows(err) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read sample: %w", err)
	}
	if sm.Time, err = parseTime(sql.NullString{String: ts, Valid: true}); err != nil {
		return nil, err
	}
	if sm.ResetsAt, err = parseTime(sql.NullString{String: resets, Valid: true}); err != nil {
		return nil, err
	}
	sm.SourceAttempt = src
	return &sm, nil
}
