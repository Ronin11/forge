// Package schema is the one home for extending the common result envelope
// with a mode's extra fields (MODES.md "Result contract"): it parses
// worker.EnvelopeSchema — the same constant the worker enforces — and adds
// properties and required entries, so the envelope shape never grows a
// second copy that could drift.
package schema

import (
	"encoding/json"
	"fmt"

	"forge/internal/core/worker"
)

// MustExtend returns the envelope schema with extraProps — a JSON object of
// property name → schema, or "" for none — merged into properties, and
// extraRequired appended to required. Every input is a package constant in a
// mode package, so a malformed one is a programming error caught by the
// mode tests; MustExtend panics rather than return an error nobody can
// handle at a call site.
func MustExtend(extraProps string, extraRequired ...string) json.RawMessage {
	var top map[string]json.RawMessage
	if err := json.Unmarshal([]byte(worker.EnvelopeSchema), &top); err != nil {
		panic(fmt.Sprintf("schema: envelope does not parse: %v", err))
	}
	var props map[string]json.RawMessage
	if err := json.Unmarshal(top["properties"], &props); err != nil {
		panic(fmt.Sprintf("schema: envelope properties do not parse: %v", err))
	}
	if extraProps != "" {
		var extra map[string]json.RawMessage
		if err := json.Unmarshal([]byte(extraProps), &extra); err != nil {
			panic(fmt.Sprintf("schema: extra properties do not parse: %v", err))
		}
		for name, s := range extra {
			if _, dup := props[name]; dup {
				panic(fmt.Sprintf("schema: extra property %q collides with the envelope", name))
			}
			props[name] = s
		}
	}
	var required []string
	if err := json.Unmarshal(top["required"], &required); err != nil {
		panic(fmt.Sprintf("schema: envelope required does not parse: %v", err))
	}
	for _, name := range extraRequired {
		if _, ok := props[name]; !ok {
			panic(fmt.Sprintf("schema: required %q is not a declared property", name))
		}
		required = append(required, name)
	}
	var err error
	if top["properties"], err = json.Marshal(props); err != nil {
		panic(fmt.Sprintf("schema: marshal properties: %v", err))
	}
	if top["required"], err = json.Marshal(required); err != nil {
		panic(fmt.Sprintf("schema: marshal required: %v", err))
	}
	out, err := json.Marshal(top)
	if err != nil {
		panic(fmt.Sprintf("schema: marshal: %v", err))
	}
	return out
}

// Findings is the finding-list schema review and audit share (MODES.md
// §audit: "findings[] as review"), so the shape has one home.
const Findings = `{"type":"array","items":{"type":"object","additionalProperties":false,"required":["file","severity","summary"],"properties":{"file":{"type":"string"},"line":{"type":"integer"},"severity":{"type":"string","enum":["high","medium","low"]},"category":{"type":"string"},"summary":{"type":"string"},"suggestion":{"type":"string"}}}}`

// Commits is the commit-list schema implement and maintain share, so the
// shape has one home.
const Commits = `{"type":"array","items":{"type":"object","additionalProperties":false,"required":["sha","subject"],"properties":{"sha":{"type":"string"},"subject":{"type":"string"}}}}`

// Scores is the 1-5 quality-rating object the learning loop aggregates
// (attempt_facts score_* columns): optional on run so any judging directive
// can emit it, required inside supervise's assessment. `overall` is the one
// mandatory axis; `weakness` names the biggest remaining gap.
const Scores = `{"type":"object","additionalProperties":false,"required":["overall"],"properties":{` +
	`"correctness":{"type":"integer","minimum":1,"maximum":5},` +
	`"completeness":{"type":"integer","minimum":1,"maximum":5},` +
	`"quality":{"type":"integer","minimum":1,"maximum":5},` +
	`"effort_fit":{"type":"integer","minimum":1,"maximum":5},` +
	`"overall":{"type":"integer","minimum":1,"maximum":5},` +
	`"weakness":{"type":"string"}}}`
