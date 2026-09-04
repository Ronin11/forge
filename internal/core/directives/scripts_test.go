package directives

import (
	"strings"
	"testing"
)

// The header grammar: keys parse with frontmatter strictness, multi-line
// input schemas join, tool: true demands description + schema, and the body
// must compile and define main.
func TestParseScriptHeader(t *testing.T) {
	good := `/**forge
 * description: Pick the busiest repo.
 * input: {"type":"object","properties":{"repos":{"type":"array"}},
 *        "required":["repos"]}
 * timeout_ms: 10000
 * tool: true
 */
function main(input) { return input.params.repos[0]; }
`
	dir := write(t, map[string]string{"scripts/busiest.js": good})
	lib, err := Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	f := lib.Script("busiest")
	if f == nil || !f.Script || !f.Tool {
		t.Fatalf("script = %+v", f)
	}
	if f.Description != "Pick the busiest repo." || f.TimeoutMS != 10000 {
		t.Errorf("meta = %q %d", f.Description, f.TimeoutMS)
	}
	if !strings.Contains(f.InputSchema, `"required":["repos"]`) || !strings.Contains(f.InputSchema, "properties") {
		t.Errorf("multi-line schema not joined: %q", f.InputSchema)
	}
	if !strings.HasPrefix(f.Body, "/**forge") {
		t.Error("body lost the header (must stay runnable as authored)")
	}
	if lib.Directive("busiest") != nil || lib.Persona("busiest") != nil {
		t.Error("kind accessors leak")
	}
}

func TestParseScriptRefusals(t *testing.T) {
	for name, src := range map[string]string{
		"unknown key":      "/**forge\n * budget: 4\n */\nfunction main(i){return 1}",
		"orphan line":      "/**forge\n * floating text\n */\nfunction main(i){return 1}",
		"dup key":          "/**forge\n * tool: false\n * tool: true\n */\nfunction main(i){return 1}",
		"bad tool":         "/**forge\n * tool: yes\n */\nfunction main(i){return 1}",
		"bad timeout":      "/**forge\n * timeout_ms: soon\n */\nfunction main(i){return 1}",
		"input not object": "/**forge\n * input: [1,2]\n */\nfunction main(i){return 1}",
		"tool no desc":     "/**forge\n * input: {\"type\":\"object\"}\n * tool: true\n */\nfunction main(i){return 1}",
		"tool no schema":   "/**forge\n * description: d\n * tool: true\n */\nfunction main(i){return 1}",
		"unterminated":     "/**forge\n * description: d\nfunction main(i){return 1}",
		"syntax error":     "function main(i){ return ; ]}",
		"no main":          "var x = 1;",
	} {
		if _, err := Load(write(t, map[string]string{"scripts/x.js": src})); err == nil {
			t.Errorf("%s: accepted", name)
		}
	}
	// Headerless non-tool script is fine; timeout clamps.
	dir := write(t, map[string]string{
		"scripts/plain.js":   "function main(input) { return 42; }",
		"scripts/clamped.js": "/**forge\n * timeout_ms: 999999\n */\nfunction main(i){return 1}",
	})
	lib, err := Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	if f := lib.Script("plain"); f == nil || f.Tool || f.Description != "" {
		t.Errorf("plain = %+v", f)
	}
	if f := lib.Script("clamped"); f.TimeoutMS != maxScriptTimeoutMS {
		t.Errorf("clamp = %d", f.TimeoutMS)
	}
	// Duplicate name across kinds is the existing library error.
	if _, err := Load(write(t, map[string]string{
		"scripts/x.js":   "function main(i){return 1}",
		"fragments/x.md": "clash",
	})); err == nil || !strings.Contains(err.Error(), "twice") {
		t.Errorf("cross-kind dup = %v", err)
	}
}

// description: is legal frontmatter on every .md kind; tool: on directives only.
func TestMarkdownDescriptionAndTool(t *testing.T) {
	dir := write(t, map[string]string{
		"directives/triage.md": "---\nmode: run\nmodel: haiku\ndescription: sweep a repo\ntool: true\n---\nbody",
		"personas/p.md":        "---\ndescription: an identity\n---\nbody",
		"fragments/f.md":       "---\ndescription: a block\n---\nbody",
	})
	lib, err := Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	if d := lib.Directive("triage"); !d.Tool || d.Description != "sweep a repo" {
		t.Errorf("directive = %+v", d)
	}
	if lib.Persona("p").Description != "an identity" || lib.Fragment("f").Description != "a block" {
		t.Error("descriptions missing")
	}
	if _, err := Load(write(t, map[string]string{"personas/p.md": "---\ntool: true\n---\nbody"})); err == nil {
		t.Error("tool: on a persona accepted")
	}
	if _, err := Load(write(t, map[string]string{"directives/d.md": "---\nmode: run\nmodel: haiku\ntool: maybe\n---\nbody"})); err == nil {
		t.Error("bad tool value accepted")
	}
}

// WithVariant handles scripts: a compiling variant swaps in, a broken one is
// refused.
func TestWithVariantScript(t *testing.T) {
	dir := write(t, map[string]string{"scripts/calc.js": "function main(i){return 1}"})
	lib, err := Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	v, err := lib.WithVariant("calc", "function main(i){return 2}")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(v.Script("calc").Body, "return 2") {
		t.Error("variant not applied")
	}
	if _, err := lib.WithVariant("calc", "function main(i){ ]"); err == nil {
		t.Error("broken script variant accepted")
	}
}
