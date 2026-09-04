package flow

import (
	"reflect"
	"testing"
)

func TestExpandTemplate(t *testing.T) {
	in := ScriptInput{
		Run: RunInfo{ID: "r1", Workflow: "wf", Objective: "ship it", Repositories: []string{"a", "b"}},
		Steps: map[string]StepInput{
			"probe": {Status: "succeeded", Output: map[string]any{"kind": "docs", "stats": map[string]any{"count": 3.0}, "flag": true}},
		},
	}
	got, missing := ExpandTemplate(
		"do {{steps.probe.output.kind}} ({{ steps.probe.output.stats.count }}, {{steps.probe.output.flag}}) on {{run.repositories}} for {{run.objective}}; keep {{objective}} and {{repo}}; miss {{steps.ghost.output.x}}{{steps.probe.output.nope}}",
		in)
	want := "do docs (3, true) on a, b for ship it; keep {{objective}} and {{repo}}; miss "
	if got != want {
		t.Errorf("got %q\nwant %q", got, want)
	}
	if !reflect.DeepEqual(missing, []string{"steps.ghost.output.x", "steps.probe.output.nope"}) {
		t.Errorf("missing = %v", missing)
	}
	if s, m := ExpandTemplate("status {{steps.probe.status}}", in); s != "status succeeded" || m != nil {
		t.Errorf("status = %q %v", s, m)
	}
}

// state and summary resolve like scripts see them; empty ones report missing.
func TestExpandTemplateStateSummary(t *testing.T) {
	in := ScriptInput{Steps: map[string]StepInput{
		"plan": {Status: "succeeded", State: "succeeded", Summary: "three tasks, gauges first"},
		"bare": {Status: "succeeded"},
	}}
	out, missing := ExpandTemplate("did {{steps.plan.state}}: {{steps.plan.summary}}", in)
	if out != "did succeeded: three tasks, gauges first" || len(missing) != 0 {
		t.Errorf("expand = %q, missing %v", out, missing)
	}
	if _, missing = ExpandTemplate("{{steps.bare.summary}}", in); len(missing) != 1 {
		t.Errorf("empty summary should report missing: %v", missing)
	}
}
