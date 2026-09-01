package modes

import (
	"strings"
	"testing"

	"forge/internal/core/model"
	"forge/internal/protocol"
)

// The verify prompt must carry the subject's claims: the verify session
// shares no context with the subject (M4 smoke 22 found it empty).
func TestVerifyWorkRendersClaims(t *testing.T) {
	env := &protocol.ResultEnvelope{
		Summary: "added a sentence to NOTES.md",
		Claims: []protocol.ResultClaim{
			{Claim: "sentence exists", Evidence: "NOTES.md:205"},
			{Claim: "checks pass", Evidence: "forge_check exit 0"},
		},
	}
	c := FollowUpContext{AttemptID: "0123456789abcdef0123456789abcdef", Repository: "forge",
		Branch: "forge/impl-x", Head: "a4ad8898", Class: model.ClassNormal, Priority: 50}
	specs := VerifyWork(env, c)
	if len(specs) != 1 {
		t.Fatalf("specs = %d", len(specs))
	}
	p := specs[0].Prompt
	for _, want := range []string{"sentence exists", "NOTES.md:205", "checks pass", "a4ad8898", "forge/impl-x", "has completed", "added a sentence"} {
		if !strings.Contains(p, want) {
			t.Errorf("prompt missing %q:\n%s", want, p)
		}
	}
	if specs[0].Class != model.ClassNormal || specs[0].Mode != "verify" {
		t.Errorf("spec = %+v", specs[0])
	}
	// No claims: the prompt says so instead of rendering an empty list.
	none := VerifyWork(&protocol.ResultEnvelope{Summary: "s"}, c)
	if !strings.Contains(none[0].Prompt, "no claims") {
		t.Errorf("no-claims prompt: %s", none[0].Prompt)
	}
}
