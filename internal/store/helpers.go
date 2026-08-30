package store

import (
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
)

// jsonOrNull stores an empty slice as NULL and anything else as JSON.
func jsonOrNull(v []string) any {
	if len(v) == 0 {
		return nil
	}
	b, err := json.Marshal(v)
	if err != nil {
		// A []string cannot fail to marshal; this keeps the signature honest.
		panic(fmt.Sprintf("store: marshal strings: %v", err))
	}
	return string(b)
}

// jsonList stores a slice as JSON even when empty ("[]"), for NOT NULL columns
// where an empty list is a valid value (a draft routine has no repositories).
func jsonList(v []string) string {
	if v == nil {
		v = []string{}
	}
	b, err := json.Marshal(v)
	if err != nil {
		panic(fmt.Sprintf("store: marshal strings: %v", err))
	}
	return string(b)
}

// jsonStrings reads what jsonOrNull wrote.
func jsonStrings(s sql.NullString) ([]string, error) {
	if !s.Valid || s.String == "" {
		return nil, nil
	}
	var out []string
	if err := json.Unmarshal([]byte(s.String), &out); err != nil {
		return nil, fmt.Errorf("decode json strings %q: %w", s.String, err)
	}
	return out, nil
}

// jsonRaw stores optional JSON documents.
func jsonRaw(b []byte) any {
	if len(b) == 0 {
		return nil
	}
	return string(b)
}

func nullInt(n int) any {
	if n == 0 {
		return nil
	}
	return n
}

func nullIntPtr(n *int) any {
	if n == nil {
		return nil
	}
	return *n
}

func nullFloat(f float64) any {
	if f == 0 {
		return nil
	}
	return f
}

func nullFloatPtr(f *float64) any {
	if f == nil {
		return nil
	}
	return *f
}

// isUniqueViolation recognises SQLite's constraint error so callers can map it
// to ErrConflict; the driver exposes it only as text.
func isUniqueViolation(err error) bool {
	return err != nil && strings.Contains(err.Error(), "UNIQUE constraint failed")
}

// oneRow turns "no rows affected" into ErrNotFound for updates keyed by id.
func oneRow(res sql.Result, what string) error {
	n, err := res.RowsAffected()
	if err != nil {
		return fmt.Errorf("rows affected for %s: %w", what, err)
	}
	if n == 0 {
		return fmt.Errorf("%s: %w", what, ErrNotFound)
	}
	return nil
}

// isNoRows is errors.Is(err, sql.ErrNoRows), named for readability at call sites.
func isNoRows(err error) bool { return errors.Is(err, sql.ErrNoRows) }
