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
)

// ParseTarget splits a routine target ("directive:<name>" | "workflow:<name>")
// into kind and name. The empty string is a legacy content routine: ("", "",
// nil).
func ParseTarget(s string) (TargetKind, string, error) {
	if s == "" {
		return "", "", nil
	}
	for _, kind := range []TargetKind{TargetDirective, TargetWorkflow} {
		prefix := string(kind) + ":"
		name, ok := strings.CutPrefix(s, prefix)
		if !ok || name == "" {
			continue
		}
		// Directive names may nest ("roles/reviewer", the library's rule);
		// workflow names are flat.
		segments := []string{name}
		if kind == TargetDirective {
			segments = strings.Split(name, "/")
		}
		for _, seg := range segments {
			if err := model.ValidateName(seg); err != nil {
				return "", "", fmt.Errorf("target %q: %w", s, err)
			}
		}
		return kind, name, nil
	}
	return "", "", fmt.Errorf("target %q must be directive:<name> or workflow:<name>", s)
}
