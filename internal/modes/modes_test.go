package modes_test

import (
	"encoding/json"
	"sort"
	"strings"
	"testing"

	"forge/internal/model"
	"forge/internal/modes"
	"forge/internal/modes/all"
	"forge/internal/protocol"
)

// want is MODES.md's per-mode table; the test fails when an implementation
// and the table drift apart.
var wantTable = map[string]struct {
	level       model.VerificationLevel
	writes      model.WriteScope
	class       model.BudgetClass
	autonomy    model.Autonomy
	checkpoints []string
	extra       string // one spot-checked schema property beyond the envelope
	required    []string
}{
	"run":        {model.L1, model.WritesRepo, model.ClassNormal, "", nil, "", nil},
	"greenfield": {model.L2, model.WritesNewProject, model.ClassNormal, model.AutonomyCheckpoint, []string{"after_spec", "after_plan", "before_report"}, "spec_note", nil},
	"intake":     {model.L0, model.WritesKbOnly, model.ClassNormal, model.AutonomyCheckpoint, []string{"before_handoff"}, "issue", nil},
	"implement":  {model.L2, model.WritesRepo, model.ClassNormal, "", []string{"before_report"}, "commits", nil},
	"review":     {model.L0, model.WritesNone, model.ClassNormal, model.AutonomyAuto, nil, "findings", []string{"verdict", "findings"}},
	"verify":     {model.L0, model.WritesNone, model.ClassNormal, model.AutonomyAuto, nil, "claims_checked", []string{"verdict"}},
	"audit":      {model.L0, model.WritesKbOnly, model.ClassBacklog, model.AutonomyAuto, nil, "note_id", nil},
	"curate":     {model.L0, model.WritesKbOnly, model.ClassBacklog, model.AutonomyAuto, nil, "note_id", nil},
	"maintain":   {model.L1, model.WritesRepo, model.ClassBacklog, "", nil, "commits", nil},
	"docs":       {model.L1, model.WritesDocsOnly, model.ClassBacklog, "", nil, "docs_updated", nil},
	"explore":    {model.L0, model.WritesKbOnly, model.ClassBacklog, model.AutonomyAuto, nil, "sources", nil},
	"retro":      {model.L0, model.WritesKbOnly, model.ClassBacklog, model.AutonomyAuto, nil, "proposals", nil},
	"plan":       {model.L0, model.WritesNone, model.ClassInteractive, "", []string{"before_report"}, "tasks", []string{"tasks"}},
	"integrate":  {model.L1, model.WritesRepo, model.ClassInteractive, model.AutonomyAuto, nil, "resolved", []string{"resolved"}},
}

var envelopeFields = []string{"schema_version", "summary", "needs_input", "changes", "checks_run", "claims"}

func TestAllMatchesTable(t *testing.T) {
	list := all.All()
	if len(list) != len(wantTable) {
		t.Fatalf("All() has %d modes, want %d", len(list), len(wantTable))
	}
	var names []string
	for _, m := range list {
		names = append(names, m.Name())
	}
	if !sort.StringsAreSorted(names) {
		t.Errorf("All() not sorted: %v", names)
	}
	if _, err := modes.NewRegistry(list); err != nil {
		t.Fatalf("NewRegistry: %v", err)
	}
	for _, m := range list {
		want, ok := wantTable[m.Name()]
		if !ok {
			t.Errorf("unexpected mode %q", m.Name())
			continue
		}
		if err := model.ValidateName(m.Name()); err != nil {
			t.Errorf("%s: %v", m.Name(), err)
		}
		if m.Level() != want.level {
			t.Errorf("%s: level %v, want %v", m.Name(), m.Level(), want.level)
		}
		if m.Writes() != want.writes {
			t.Errorf("%s: writes %v, want %v", m.Name(), m.Writes(), want.writes)
		}
		if m.DefaultClass() != want.class {
			t.Errorf("%s: class %v, want %v", m.Name(), m.DefaultClass(), want.class)
		}
		if m.DefaultAutonomy() != want.autonomy {
			t.Errorf("%s: autonomy %q, want %q", m.Name(), m.DefaultAutonomy(), want.autonomy)
		}
		if a := m.DefaultAutonomy(); a != "" && !a.Valid() {
			t.Errorf("%s: invalid autonomy %q", m.Name(), a)
		}
		if got := m.Checkpoints(); strings.Join(got, ",") != strings.Join(want.checkpoints, ",") {
			t.Errorf("%s: checkpoints %v, want %v", m.Name(), got, want.checkpoints)
		}
	}
}

func TestPreambles(t *testing.T) {
	for _, m := range all.All() {
		p := m.Preamble()
		if p == "" {
			t.Errorf("%s: empty preamble", m.Name())
			continue
		}
		if !strings.Contains(p, modes.ConstitutionNine) {
			t.Errorf("%s: preamble lacks constitution 9 verbatim", m.Name())
		}
		if !strings.Contains(p, "JSON") {
			t.Errorf("%s: preamble lacks the JSON result contract", m.Name())
		}
	}
}

func TestSchemasExtendEnvelope(t *testing.T) {
	for _, m := range all.All() {
		var top struct {
			Properties map[string]json.RawMessage `json:"properties"`
			Required   []string                   `json:"required"`
		}
		if err := json.Unmarshal(m.ResultSchema(), &top); err != nil {
			t.Errorf("%s: schema does not parse: %v", m.Name(), err)
			continue
		}
		for _, f := range envelopeFields {
			if _, ok := top.Properties[f]; !ok {
				t.Errorf("%s: schema lacks envelope field %q", m.Name(), f)
			}
		}
		req := map[string]bool{}
		for _, r := range top.Required {
			req[r] = true
		}
		for _, f := range envelopeFields {
			if !req[f] {
				t.Errorf("%s: envelope field %q not required", m.Name(), f)
			}
		}
		want := wantTable[m.Name()]
		if want.extra == "" {
			if len(top.Properties) != len(envelopeFields) {
				t.Errorf("%s: expected envelope-only schema, got %d properties", m.Name(), len(top.Properties))
			}
		} else if _, ok := top.Properties[want.extra]; !ok {
			t.Errorf("%s: schema lacks extra field %q", m.Name(), want.extra)
		}
		for _, r := range want.required {
			if !req[r] {
				t.Errorf("%s: extra field %q not required", m.Name(), r)
			}
		}
	}
}

func TestAllowedTools(t *testing.T) {
	for _, m := range all.All() {
		tools := m.AllowedTools()
		seen := map[string]bool{}
		for _, tool := range tools {
			if seen[tool] {
				t.Errorf("%s: duplicate tool %q", m.Name(), tool)
			}
			seen[tool] = true
		}
		for _, core := range []string{"forge_note_progress", "forge_kb_search", "forge_usage"} {
			if !seen[core] {
				t.Errorf("%s: missing universal tool %q", m.Name(), core)
			}
		}
		if m.Name() == "retro" {
			if tools[0] != modes.SentinelNoBuiltins {
				t.Errorf("retro: first tool %q, want the %q sentinel", tools[0], modes.SentinelNoBuiltins)
			}
			for _, tool := range tools[1:] {
				if !strings.HasPrefix(tool, "forge_") {
					t.Errorf("retro: non-forge tool %q in a tools-only mode", tool)
				}
			}
		}
	}
}

func TestSeeds(t *testing.T) {
	list := all.All()
	seeds := modes.Seeds(list)
	if len(seeds) != len(list) {
		t.Fatalf("Seeds has %d entries, want %d", len(seeds), len(list))
	}
	for _, m := range list {
		if seeds[m.Name()] != m.Preamble() {
			t.Errorf("Seeds[%q] is not the mode's preamble", m.Name())
		}
	}
}

func registry(t *testing.T) *modes.Registry {
	t.Helper()
	r, err := modes.NewRegistry(all.All())
	if err != nil {
		t.Fatal(err)
	}
	return r
}

func TestIntakeFollowUps(t *testing.T) {
	intake := registry(t).Get("intake")
	c := modes.FollowUpContext{
		AttemptID:  "0123456789abcdef0123456789abcdef",
		Repository: "myrepo",
		Class:      model.ClassNormal,
		Priority:   3,
	}
	env := &protocol.ResultEnvelope{Extra: map[string]json.RawMessage{
		"handoff": json.RawMessage(`{"requested":true,"mode":"implement"}`),
		"issue":   json.RawMessage(`{"title":"fix the flake","body":"## Summary\nflaky test"}`),
	}}
	specs := intake.FollowUps(env, c)
	if len(specs) != 1 {
		t.Fatalf("got %d specs, want 1", len(specs))
	}
	s := specs[0]
	if s.Mode != "implement" || s.Repository != "myrepo" || s.Priority != 3 || s.Class != model.ClassNormal {
		t.Errorf("spec routing wrong: %+v", s)
	}
	if s.Prompt != "## Summary\nflaky test" || s.Title != "fix the flake" {
		t.Errorf("spec content wrong: %+v", s)
	}
	if s.Autonomy != "" {
		t.Errorf("autonomy %q, want empty (resolver decides)", s.Autonomy)
	}

	// No handoff requested → nothing.
	env.Extra["handoff"] = json.RawMessage(`{"requested":false}`)
	if got := intake.FollowUps(env, c); got != nil {
		t.Errorf("unrequested handoff spawned %v", got)
	}
	// Nil Extra (daemon did not decode extras) → nothing.
	if got := intake.FollowUps(&protocol.ResultEnvelope{}, c); got != nil {
		t.Errorf("nil Extra spawned %v", got)
	}
}

func TestL2FollowUps(t *testing.T) {
	c := modes.FollowUpContext{
		AttemptID:  "0123456789abcdef0123456789abcdef",
		Repository: "myrepo",
		Branch:     "forge/nightly-01234567",
		Head:       "deadbeef",
		Class:      model.ClassBacklog,
		Priority:   2,
		UI:         true,
	}
	reg := registry(t)
	for _, m := range all.All() {
		specs := m.FollowUps(&protocol.ResultEnvelope{}, c)
		if m.Level() != model.L2 {
			if m.Name() != "intake" && specs != nil {
				t.Errorf("%s: non-L2 mode spawned %v", m.Name(), specs)
			}
			continue
		}
		if len(specs) != 1 {
			t.Fatalf("%s: got %d specs, want 1", m.Name(), len(specs))
		}
		s := specs[0]
		if s.Mode != "verify" || s.Repository != "myrepo" || s.Priority != 2 {
			t.Errorf("%s: verify routing wrong: %+v", m.Name(), s)
		}
		if s.Class != model.ClassNormal {
			t.Errorf("%s: backlog subject got class %q, want normal", m.Name(), s.Class)
		}
		if s.Title != "verify 01234567" {
			t.Errorf("%s: title %q", m.Name(), s.Title)
		}
		if s.Autonomy != model.AutonomyAuto || s.Timeout != 1200 || s.MaxTurns != 30 {
			t.Errorf("%s: verify knobs wrong: %+v", m.Name(), s)
		}
		if s.VerifyOf == nil || s.VerifyOf.AttemptID != c.AttemptID || s.VerifyOf.Branch != c.Branch ||
			s.VerifyOf.Head != "deadbeef" || !s.VerifyOf.UI {
			t.Errorf("%s: VerifyOf wrong: %+v", m.Name(), s.VerifyOf)
		}
	}
	// An interactive subject's verification is interactive.
	c.Class = model.ClassInteractive
	specs := reg.Get("implement").FollowUps(nil, c)
	if len(specs) != 1 || specs[0].Class != model.ClassInteractive {
		t.Errorf("interactive subject: %+v", specs)
	}
}
