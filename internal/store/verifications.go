package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	"forge/internal/model"
	"forge/internal/protocol"
)

// Verification is one verifications row (DESIGN.md §3 "Verification, Artifact"):
// attempt_id is always the subject, verifier_attempt_id the verify attempt for
// L2 (empty otherwise), decided_by who ran the level (worker, verify, a human).
type Verification struct {
	ID                string          `json:"id"`
	AttemptID         string          `json:"attempt_id"`
	Level             int             `json:"level"`
	Passed            bool            `json:"passed"`
	VerifierAttemptID string          `json:"verifier_attempt_id,omitempty"`
	Verdict           json.RawMessage `json:"verdict,omitempty"`
	DecidedBy         string          `json:"decided_by,omitempty"`
	CreatedAt         time.Time       `json:"created_at"`
}

// RecordVerification writes one row for the level attempted and keeps the
// attempt's summary columns (verification_level/_passed) in step: the highest
// level attempted wins the summary.
func (tx *Tx) RecordVerification(ctx context.Context, attemptID string, level int, passed bool, verifierAttemptID, decidedBy string, verdict json.RawMessage) error {
	if _, err := tx.Exec(ctx, `INSERT INTO verifications (id, attempt_id, level, passed, verifier_attempt_id, verdict, decided_by, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
		model.NewID(), attemptID, level, boolInt(passed), nullString(verifierAttemptID), jsonRaw(verdict), nullString(decidedBy), formatTime(tx.now)); err != nil {
		return fmt.Errorf("record verification of %s: %w", attemptID, err)
	}
	if _, err := tx.Exec(ctx, `UPDATE attempts SET verification_level = ?, verification_passed = ?, updated_at = ? WHERE id = ? AND (verification_level IS NULL OR verification_level <= ?)`,
		level, boolInt(passed), formatTime(tx.now), attemptID, level); err != nil {
		return fmt.Errorf("summarise verification of %s: %w", attemptID, err)
	}
	return tx.Journal(ctx, "attempt.verified", EntityAttempt, attemptID, map[string]any{
		"level": level, "passed": passed, "verifier_attempt_id": verifierAttemptID, "decided_by": decidedBy,
	})
}

// VerificationsForAttempt returns the subject's rows, oldest first.
func (s *Store) VerificationsForAttempt(ctx context.Context, attemptID string) ([]Verification, error) {
	var out []Verification
	err := each(s.query(ctx, `SELECT id, attempt_id, level, passed, verifier_attempt_id, verdict, decided_by, created_at FROM verifications WHERE attempt_id = ? ORDER BY rowid`, attemptID))(func(rows *sql.Rows) error {
		var v Verification
		var passed int
		var verifier, verdict, decidedBy sql.NullString
		var created string
		if err := rows.Scan(&v.ID, &v.AttemptID, &v.Level, &passed, &verifier, &verdict, &decidedBy, &created); err != nil {
			return fmt.Errorf("scan verification: %w", err)
		}
		v.Passed, v.VerifierAttemptID, v.DecidedBy = passed == 1, verifier.String, decidedBy.String
		if verdict.Valid {
			v.Verdict = json.RawMessage(verdict.String)
		}
		var err error
		if v.CreatedAt, err = parseTime(sql.NullString{String: created, Valid: true}); err != nil {
			return err
		}
		out = append(out, v)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read verifications: %w", err)
	}
	return out, nil
}

// Artifact is one artifacts row: a file a verify attempt stored, recorded by
// size and SHA-256; the bytes stay on the worker's disk.
type Artifact struct {
	ID        string    `json:"id"`
	AttemptID string    `json:"attempt_id"`
	Kind      string    `json:"kind"`
	Path      string    `json:"path"`
	Bytes     int64     `json:"bytes"`
	SHA256    string    `json:"sha256"`
	CreatedAt time.Time `json:"created_at"`
}

// RecordArtifacts inserts what an attempt reported from its artifacts
// directory, journalled once for the batch.
func (tx *Tx) RecordArtifacts(ctx context.Context, attemptID string, uploads []protocol.ArtifactUpload) error {
	if len(uploads) == 0 {
		return nil
	}
	for _, u := range uploads {
		if _, err := tx.Exec(ctx, `INSERT INTO artifacts (id, attempt_id, kind, path, bytes, sha256, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)`,
			model.NewID(), attemptID, u.Kind, u.Path, u.Bytes, u.SHA256, formatTime(tx.now)); err != nil {
			return fmt.Errorf("record artifact %s of %s: %w", u.Path, attemptID, err)
		}
	}
	return tx.Journal(ctx, "attempt.artifacts", EntityAttempt, attemptID, map[string]any{"count": len(uploads)})
}

// ArtifactsForAttempt lists an attempt's recorded artifacts, oldest first.
func (s *Store) ArtifactsForAttempt(ctx context.Context, attemptID string) ([]Artifact, error) {
	var out []Artifact
	err := each(s.query(ctx, `SELECT id, attempt_id, kind, path, bytes, sha256, created_at FROM artifacts WHERE attempt_id = ? ORDER BY rowid`, attemptID))(func(rows *sql.Rows) error {
		var a Artifact
		var created string
		if err := rows.Scan(&a.ID, &a.AttemptID, &a.Kind, &a.Path, &a.Bytes, &a.SHA256, &created); err != nil {
			return fmt.Errorf("scan artifact: %w", err)
		}
		var err error
		if a.CreatedAt, err = parseTime(sql.NullString{String: created, Valid: true}); err != nil {
			return err
		}
		out = append(out, a)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read artifacts: %w", err)
	}
	return out, nil
}
