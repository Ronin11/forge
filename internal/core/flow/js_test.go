package flow

import (
	"strings"
	"testing"
	"time"
)

func TestRunScriptBasics(t *testing.T) {
	in := ScriptInput{
		Run:   RunInfo{ID: "r1", Workflow: "wf", Objective: "ship it"},
		Steps: map[string]StepInput{"build": {Status: "succeeded", Iteration: 1, Output: map[string]any{"kind": "docs", "count": 3.0}}},
	}
	out, err := RunScript(`function main(input) {
		return {kind: input.steps.build.output.kind, doubled: input.steps.build.output.count * 2, obj: input.run.objective}
	}`, in, time.Second)
	if err != nil {
		t.Fatal(err)
	}
	want := `{"doubled":6,"kind":"docs","obj":"ship it"}`
	if string(out) != want {
		t.Errorf("out = %s, want %s", out, want)
	}
}

func TestRunScriptNoMain(t *testing.T) {
	if _, err := RunScript(`var x = 1`, ScriptInput{}, time.Second); err == nil || !strings.Contains(err.Error(), "main") {
		t.Errorf("err = %v", err)
	}
}

func TestRunScriptThrow(t *testing.T) {
	if _, err := RunScript(`function main() { throw new Error("boom") }`, ScriptInput{}, time.Second); err == nil || !strings.Contains(err.Error(), "boom") {
		t.Errorf("err = %v", err)
	}
}

func TestRunScriptTimeout(t *testing.T) {
	start := time.Now()
	_, err := RunScript(`function main() { while (true) {} }`, ScriptInput{}, 100*time.Millisecond)
	if err == nil || !strings.Contains(err.Error(), "timeout") {
		t.Fatalf("err = %v", err)
	}
	if time.Since(start) > 3*time.Second {
		t.Errorf("interrupt took %s", time.Since(start))
	}
}

func TestRunScriptNoHostAccess(t *testing.T) {
	// The sandbox has no require/process/fetch/fs — referencing them throws.
	for _, src := range []string{
		`function main() { return require("fs") }`,
		`function main() { return process.env }`,
		`function main() { return fetch("http://x") }`,
	} {
		if _, err := RunScript(src, ScriptInput{}, time.Second); err == nil {
			t.Errorf("%s: no error", src)
		}
	}
}

func TestRunScriptOutputCap(t *testing.T) {
	if _, err := RunScript(`function main() { return "x".repeat(100000) }`, ScriptInput{}, time.Second); err == nil || !strings.Contains(err.Error(), "exceeds") {
		t.Errorf("err = %v", err)
	}
}

func TestEvalSwitch(t *testing.T) {
	in := ScriptInput{Steps: map[string]StepInput{"triage": {Output: map[string]any{"verdict": "retry"}}}}
	got, err := EvalSwitch(`input.steps.triage.output.verdict`, in, time.Second)
	if err != nil || got != "retry" {
		t.Fatalf("got %q, %v", got, err)
	}
	if got, err = EvalSwitch(`1 + 1`, in, time.Second); err != nil || got != "2" {
		t.Fatalf("got %q, %v", got, err)
	}
	if _, err = EvalSwitch(`nonsense.path`, in, time.Second); err == nil {
		t.Error("bad expression evaluated")
	}
}

func TestCompileScript(t *testing.T) {
	if err := CompileScript(`function main(input) { return 1 }`); err != nil {
		t.Error(err)
	}
	if err := CompileScript(`function main( {`); err == nil {
		t.Error("syntax error compiled")
	}
}
