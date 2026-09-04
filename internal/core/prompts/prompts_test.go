package prompts

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// write lays out a prompts tree in a temp dir (no git — manifests must still
// work, flagged dirty).
func write(t *testing.T, files map[string]string) string {
	t.Helper()
	dir := t.TempDir()
	for name, content := range files {
		path := filepath.Join(dir, name)
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	return dir
}

func TestResolveComposesIncludesModesAndParams(t *testing.T) {
	dir := write(t, map[string]string{
		"personas/reviewer.md": `---
model: opus
---
You are the reviewer.
{{> standards}}
{{> house-style/lang lang="Go"}}

## mode: review
Only flag what matters.
{{> verdict}}

## mode: implement
Ship small diffs.`,
		"fragments/standards.md":        "Be honest, not flattering.\n{{> depth-two}}",
		"fragments/depth-two.md":        "Extend, don't duplicate.",
		"fragments/house-style/lang.md": "Follow {{lang}} idioms. Leave {{objective}} alone.",
		"fragments/verdict.md":          "State a verdict.",
	})
	lib, err := Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	if p := lib.Persona("reviewer"); p == nil || p.Model != "opus" {
		t.Fatalf("persona = %+v", p)
	}

	text, comp, err := lib.Resolve("reviewer", "review")
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{
		"You are the reviewer.",
		"Be honest, not flattering.",
		"Extend, don't duplicate.",   // nested include
		"Follow Go idioms.",          // param substituted
		"Leave {{objective}} alone.", // foreign placeholder untouched
		"Only flag what matters.",    // review mode section
		"State a verdict.",           // include inside the mode section
	} {
		if !strings.Contains(text, want) {
			t.Errorf("resolved text missing %q\n%s", want, text)
		}
	}
	if strings.Contains(text, "Ship small diffs.") {
		t.Error("implement-mode section leaked into a review resolve")
	}
	if strings.Contains(text, "{{>") {
		t.Errorf("unexpanded include remains:\n%s", text)
	}
	if comp.Persona != "reviewer" || comp.Mode != "review" || !comp.Dirty || comp.Commit != "" {
		t.Errorf("composition = %+v (no git: want dirty, no commit)", comp)
	}
	names := make([]string, len(comp.Fragments))
	for i, f := range comp.Fragments {
		names[i] = f.Name
		if len(f.Hash) != 64 {
			t.Errorf("fragment %s hash = %q", f.Name, f.Hash)
		}
	}
	if got := strings.Join(names, ","); got != "depth-two,house-style/lang,reviewer,standards,verdict" {
		t.Errorf("manifest = %s", got)
	}

	// An unknown mode composes the core only — modes vary per routine.
	text, _, err = lib.Resolve("reviewer", "docs")
	if err != nil || strings.Contains(text, "Only flag") || !strings.Contains(text, "You are the reviewer.") {
		t.Errorf("unknown mode resolve = %q, %v", text, err)
	}
}

func TestLoadRefusesBrokenTrees(t *testing.T) {
	cases := []struct {
		name  string
		files map[string]string
		want  string
	}{
		{"unknown include", map[string]string{
			"personas/p.md": "{{> ghost}}",
		}, "not found"},
		{"cycle", map[string]string{
			"fragments/a.md": "{{> b}}",
			"fragments/b.md": "{{> a}}",
		}, "cycle"},
		{"self include", map[string]string{
			"fragments/a.md": "{{> a}}",
		}, "cycle"},
		{"bad frontmatter key", map[string]string{
			"fragments/a.md": "---\ntools: all\n---\nbody",
		}, "unknown frontmatter key"},
		{"unterminated frontmatter", map[string]string{
			"fragments/a.md": "---\nmodel: x",
		}, "unterminated"},
		{"bad name", map[string]string{
			"fragments/Bad Name.md": "body",
		}, "slug"},
		{"broken include in a mode section", map[string]string{
			"personas/p.md": "core\n\n## mode: review\n{{> ghost}}",
		}, "not found"},
	}
	for _, tc := range cases {
		dir := write(t, tc.files)
		if _, err := Load(dir); err == nil || !strings.Contains(err.Error(), tc.want) {
			t.Errorf("%s: err = %v, want %q", tc.name, err, tc.want)
		}
	}
}

func TestResolveUnknownPersona(t *testing.T) {
	dir := write(t, map[string]string{"fragments/a.md": "not a persona"})
	lib, err := Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	if _, _, err := lib.Resolve("a", ""); err == nil {
		t.Error("a fragment resolved as a persona")
	}
	if _, _, err := lib.Resolve("ghost", ""); err == nil {
		t.Error("a missing persona resolved")
	}
}

func TestEnsureBootstrapsAndIsIdempotent(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "prompts")
	if err := Ensure(dir); err != nil {
		t.Fatal(err)
	}
	for _, p := range []string{"personas", "fragments", ".git", "README.md"} {
		if _, err := os.Stat(filepath.Join(dir, p)); err != nil {
			t.Errorf("missing %s: %v", p, err)
		}
	}
	// A second Ensure never touches existing content.
	marker := filepath.Join(dir, "README.md")
	if err := os.WriteFile(marker, []byte("mine"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := Ensure(dir); err != nil {
		t.Fatal(err)
	}
	got, err := os.ReadFile(marker)
	if err != nil || string(got) != "mine" {
		t.Errorf("README overwritten: %q, %v", got, err)
	}
	// The starter tree loads fine.
	if _, err := Load(dir); err != nil {
		t.Errorf("starter tree load: %v", err)
	}
}

// The shipped starter library must itself be sound: it loads, every persona
// resolves for its declared modes with no unexpanded includes or dangling
// parameters, and the bootstrap commit leaves the tree clean.
func TestStarterLibraryResolves(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "prompts")
	if err := Ensure(dir); err != nil {
		t.Fatal(err)
	}
	lib, err := Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	personas := 0
	for _, f := range lib.Fragments() {
		if !f.Persona {
			continue
		}
		personas++
		if f.Model == "" {
			t.Errorf("persona %s: no default model", f.Name)
		}
		modes := []string{""}
		for m := range f.Modes {
			modes = append(modes, m)
		}
		for _, mode := range modes {
			text, comp, err := lib.Resolve(f.Name, mode)
			if err != nil {
				t.Errorf("%s (mode %q): %v", f.Name, mode, err)
				continue
			}
			if strings.Contains(text, "{{>") {
				t.Errorf("%s (mode %q): unexpanded include", f.Name, mode)
			}
			if strings.Contains(text, "{{what}}") {
				t.Errorf("%s (mode %q): dangling parameter", f.Name, mode)
			}
			if len(comp.Fragments) < 2 {
				t.Errorf("%s: composes only %d fragments — a persona should share fragments", f.Name, len(comp.Fragments))
			}
		}
	}
	if personas < 8 {
		t.Errorf("starter library has %d personas; want the standard roles", personas)
	}
	if lib.Commit == "" || lib.Dirty {
		t.Errorf("bootstrap should leave a clean commit: commit=%q dirty=%v", lib.Commit, lib.Dirty)
	}
}

// The bootstrapped README documents every construct and variable — the
// reference lives next to the files it governs, so drift is a test failure.
func TestReadmeDocumentsTheSurface(t *testing.T) {
	for _, want := range []string{
		"{{> name}}", `{{> name key="value"}}`, "## mode:",
		"{{objective}}", "{{repo}}",
		"{{run.objective}}", "{{run.repositories}}", "{{run.workflow}}", "{{run.id}}",
		"{{steps.<node>.status}}", "{{steps.<node>.output.<dot.path>}}",
		"model:", "forge persona show",
	} {
		if !strings.Contains(readmeContent, want) {
			t.Errorf("README missing %q", want)
		}
	}
}

// WithVariant composes a candidate edit in memory: the variant resolves with
// the rest of the library untouched, and a variant that breaks composition is
// refused without touching anything.
func TestWithVariant(t *testing.T) {
	dir := write(t, map[string]string{
		"personas/p.md":       "Old core.\n{{> shared}}",
		"fragments/shared.md": "Shared text.",
	})
	lib, err := Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	v, err := lib.WithVariant("p", "New core.\n{{> shared}}\n\n## mode: run\nNew mode text.")
	if err != nil {
		t.Fatal(err)
	}
	text, _, err := v.Resolve("p", "run")
	if err != nil || !strings.Contains(text, "New core.") || !strings.Contains(text, "Shared text.") || !strings.Contains(text, "New mode text.") {
		t.Fatalf("variant resolve = %q, %v", text, err)
	}
	// The original library is untouched.
	text, _, err = lib.Resolve("p", "")
	if err != nil || !strings.Contains(text, "Old core.") {
		t.Errorf("original mutated: %q, %v", text, err)
	}
	// A broken variant is refused; varying a fragment revalidates its users.
	if _, err := lib.WithVariant("p", "{{> ghost}}"); err == nil {
		t.Error("broken variant accepted")
	}
	if _, err := lib.WithVariant("shared", "{{> p-cycle-missing}}"); err == nil {
		t.Error("fragment variant breaking a persona accepted")
	}
	if _, err := lib.WithVariant("ghost", "x"); err == nil {
		t.Error("unknown fragment accepted")
	}
}

// Directives: frontmatter carries mode (required), persona, model, effort;
// the body expands includes; `## mode:` headings in a directive body are just
// markdown.
func TestDirectives(t *testing.T) {
	dir := write(t, map[string]string{
		"directives/triage.md":   "---\nmode: run\npersona: triager\nmodel: haiku\neffort: low\n---\nTriage {{repo}}: {{objective}}\n{{> checklist}}",
		"directives/plain.md":    "---\nmode: plan\n---\nJust plan.\n\n## mode: run\nnot a section",
		"personas/triager.md":    "You triage.",
		"fragments/checklist.md": "- look at the queue",
	})
	lib, err := Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	d := lib.Directive("triage")
	if d == nil || d.Mode != "run" || d.PersonaRef != "triager" || d.Model != "haiku" || d.Effort != "low" {
		t.Fatalf("directive = %+v", d)
	}
	if lib.Directive("triager") != nil || lib.Persona("triage") != nil {
		t.Error("kind accessors leak across kinds")
	}
	text, manifest, err := lib.ResolveDirectiveBody("triage")
	if err != nil || !strings.Contains(text, "- look at the queue") || !strings.Contains(text, "Triage {{repo}}: {{objective}}") {
		t.Fatalf("resolved = %q, %v", text, err)
	}
	if len(manifest) != 2 {
		t.Errorf("manifest = %+v", manifest)
	}
	// A directive body keeps `## mode:` lines verbatim.
	if p := lib.Directive("plain"); !strings.Contains(p.Body, "## mode: run") || len(p.Modes) != 0 {
		t.Errorf("directive body mode-split: %+v", p)
	}
	if _, _, err := lib.ResolveDirectiveBody("ghost"); err == nil {
		t.Error("unknown directive resolved")
	}
}

func TestDirectiveLoadRefusals(t *testing.T) {
	for name, files := range map[string]map[string]string{
		"missing mode":       {"directives/x.md": "no frontmatter"},
		"unknown key":        {"directives/x.md": "---\nmode: run\nbudget: high\n---\nbody"},
		"bad persona name":   {"directives/x.md": "---\nmode: run\npersona: Bad Name\n---\nbody"},
		"broken include":     {"directives/x.md": "---\nmode: run\n---\n{{> ghost}}"},
		"dup vs fragment":    {"directives/x.md": "---\nmode: run\n---\nbody", "fragments/x.md": "clash"},
		"mode on fragment":   {"fragments/x.md": "---\nmode: run\n---\nbody"},
		"persona on persona": {"personas/x.md": "---\npersona: y\n---\nbody"},
	} {
		if _, err := Load(write(t, files)); err == nil {
			t.Errorf("%s: accepted", name)
		}
	}
}

// WithVariant re-validates directives too — an edit that breaks a directive's
// includes is refused, and a directive variant composes.
func TestWithVariantDirective(t *testing.T) {
	dir := write(t, map[string]string{
		"directives/triage.md": "---\nmode: run\n---\nOld task. {{> shared}}",
		"fragments/shared.md":  "Shared.",
	})
	lib, err := Load(dir)
	if err != nil {
		t.Fatal(err)
	}
	v, err := lib.WithVariant("triage", "---\nmode: run\n---\nNew task. {{> shared}}")
	if err != nil {
		t.Fatal(err)
	}
	if text, _, err := v.ResolveDirectiveBody("triage"); err != nil || !strings.Contains(text, "New task. Shared.") {
		t.Fatalf("variant = %q, %v", text, err)
	}
	if _, err := lib.WithVariant("triage", "---\nmode: run\n---\n{{> ghost}}"); err == nil {
		t.Error("broken directive variant accepted")
	}
	// Varying a fragment a directive uses re-validates the directive.
	if _, err := lib.WithVariant("shared", "{{> missing}}"); err == nil {
		t.Error("fragment variant breaking a directive accepted")
	}
}
