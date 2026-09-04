package store

import (
	"fmt"
	"strings"

	"forge/internal/core/model"
)

// TargetKind is what a trigger routine invokes.
type TargetKind string

// Target kinds.
const (
	TargetDirective TargetKind = "directive" // a directives-library file, run as one Work
	TargetWorkflow  TargetKind = "workflow"  // a workflow, run as a workflow run
	TargetScript    TargetKind = "script"    // a scripts-library file, run as a one-node run
)

// ParseTarget splits a routine target ("directive:<name>" | "workflow:<name>"
// | "script:<name>") into kind and name; "" returns ("", "", nil).
func ParseTarget(s string) (TargetKind, string, error) {
	if s == "" {
		return "", "", nil
	}
	for _, kind := range []TargetKind{TargetDirective, TargetWorkflow, TargetScript} {
		prefix := string(kind) + ":"
		name, ok := strings.CutPrefix(s, prefix)
		if !ok || name == "" {
			continue
		}
		// Library names may nest ("roles/reviewer", the library's rule);
		// workflow names are flat.
		segments := []string{name}
		if kind == TargetDirective || kind == TargetScript {
			segments = strings.Split(name, "/")
		}
		for _, seg := range segments {
			if err := model.ValidateName(seg); err != nil {
				return "", "", fmt.Errorf("target %q: %w", s, err)
			}
		}
		return kind, name, nil
	}
	return "", "", fmt.Errorf("target %q must be directive:<name>, workflow:<name>, or script:<name>", s)
}
