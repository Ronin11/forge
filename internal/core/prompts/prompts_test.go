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
	// An empty tree loads fine.
	if _, err := Load(dir); err != nil {
		t.Errorf("empty tree load: %v", err)
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
