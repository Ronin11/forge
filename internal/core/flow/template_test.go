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
