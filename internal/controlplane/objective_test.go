package controlplane

import "testing"

func TestInjectObjective(t *testing.T) {
	if got := injectObjective("Implement {{objective}} on {{repo}}", "add a --json flag"); got != "Implement add a --json flag on {{repo}}" {
		t.Errorf("substitution: %q", got)
	}
	if got := injectObjective("no placeholder here", "x"); got != "no placeholder here" {
		t.Errorf("prompt without placeholder should be unchanged: %q", got)
	}
	empty := injectObjective("do {{objective}}", "")
	if empty == "do {{objective}}" || empty == "do " {
		t.Errorf("empty objective should get a self-directed fallback: %q", empty)
	}
}
