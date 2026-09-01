package web

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// A model-class overlay is composed into the hashable template; the escalation
// note is composed only into the rendered prompt.
func TestAssemblePromptClassOverlay(t *testing.T) {
	home := t.TempDir()
	if err := os.MkdirAll(filepath.Join(home, "modes"), 0o700); err != nil {
		t.Fatal(err)
	}
	overlay := "SMALL-MODEL GUIDANCE: keep the diff tiny."
	if err := os.WriteFile(filepath.Join(home, "modes", "run.small.md"), []byte(overlay), 0o600); err != nil {
		t.Fatal(err)
	}
	base := promptInput{Mode: "run", ModePreamble: "PREAMBLE", RoutinePrompt: "do the thing", Repository: "r", Home: home, AttemptID: "a1"}

	plainTpl, _ := assemblePrompt(base)
	if strings.Contains(plainTpl, overlay) {
		t.Fatalf("no-class template should not carry the overlay:\n%s", plainTpl)
	}

	withClass := base
	withClass.ModelClass = "small"
	classTpl, classRendered := assemblePrompt(withClass)
	if !strings.Contains(classTpl, overlay) {
		t.Fatalf("class template should carry the overlay:\n%s", classTpl)
	}
	if classTpl == plainTpl {
		t.Fatal("class overlay must change the template (and thus the prompt version hash)")
	}
	if !strings.Contains(classRendered, overlay) {
		t.Error("rendered prompt should include the composed template")
	}

	// A class with no overlay file leaves the template untouched.
	frontier := base
	frontier.ModelClass = "frontier"
	if tpl, _ := assemblePrompt(frontier); tpl != plainTpl {
		t.Errorf("absent overlay must not change the template:\n%s", tpl)
	}
}

func TestAssemblePromptEscalationRenderedOnly(t *testing.T) {
	base := promptInput{Mode: "run", ModePreamble: "PREAMBLE", RoutinePrompt: "task", Repository: "r", AttemptID: "a1"}
	esc := base
	esc.EscalationNote = "ESCALATION: previous attempt failed check build."
	tpl, rendered := assemblePrompt(esc)
	if strings.Contains(tpl, "ESCALATION") {
		t.Errorf("escalation note must stay out of the hashable template:\n%s", tpl)
	}
	if !strings.Contains(rendered, "ESCALATION") {
		t.Errorf("escalation note must appear in the rendered prompt:\n%s", rendered)
	}
	// The template equals the no-escalation template — the hash is unchanged.
	plainTpl, _ := assemblePrompt(base)
	if tpl != plainTpl {
		t.Error("escalation must not change the template hash")
	}
}
