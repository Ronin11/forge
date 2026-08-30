// Package model holds Forge's pure rules: identifiers, the Target state machine,
// Work-state derivation, reasons, autonomy resolution, and dependency checks. It
// imports nothing from Forge and has no I/O, so every rule here has exactly one
// home and is tested by table.
package model

import (
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"regexp"
)

// IDs are 32 lower-case hex characters; short IDs are the first 8. They become
// paths and branch names, so anything received over the wire is validated first.
const (
	idBytes     = 16
	ShortIDSize = 8
)

var (
	idPattern   = regexp.MustCompile(`^[0-9a-f]{32}$`)
	namePattern = regexp.MustCompile(`^[a-z0-9][a-z0-9-]{0,39}$`)
)

// NewID returns a fresh random ID. crypto/rand failing is not something Forge can
// work around; it panics rather than mint a predictable ID.
func NewID() string {
	var b [idBytes]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic(fmt.Sprintf("model: crypto/rand unavailable: %v", err))
	}
	return hex.EncodeToString(b[:])
}

// ValidateID rejects anything that is not exactly a Forge ID, so an ID can be
// used in a path or ref name without further escaping.
func ValidateID(id string) error {
	if !idPattern.MatchString(id) {
		return fmt.Errorf("invalid id %q: want 32 lower-case hex characters", id)
	}
	return nil
}

// ShortID is the human-facing prefix used in branch names and the UI.
func ShortID(id string) string {
	if len(id) < ShortIDSize {
		return id
	}
	return id[:ShortIDSize]
}

// ValidateName is the one rule for routine, repository, project, worker, and
// plugin names: they become branch slugs, URL path segments, TOML keys, and fact
// links, so they are restricted to a safe alphabet and are already slugs.
func ValidateName(name string) error {
	if !namePattern.MatchString(name) {
		return fmt.Errorf("invalid name %q: want ^[a-z0-9][a-z0-9-]{0,39}$", name)
	}
	return nil
}

// BranchName is the one home for the branch an attempt works on.
func BranchName(routineName, attemptID string) string {
	return "forge/" + routineName + "-" + ShortID(attemptID)
}
