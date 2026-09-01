package controlplane

import (
	"net/http"
	"strings"
	"testing"

	"forge/internal/core/model"
	"forge/internal/modes"
	modesall "forge/internal/modes/all"
)

// Every shipped role template must name a real mode, a model, a valid class,
// and a non-empty prompt — a broken template would make the picker file bad
// routines.
func TestRoutineTemplatesWellFormed(t *testing.T) {
	reg, err := modes.NewRegistry(modesall.All())
	if err != nil {
		t.Fatal(err)
	}
	if len(routineTemplates) == 0 {
		t.Fatal("no routine templates")
	}
	seen := map[string]bool{}
	for _, tpl := range routineTemplates {
		if tpl.Key == "" || tpl.Role == "" || tpl.Prompt == "" || tpl.Model == "" || tpl.Description == "" {
			t.Errorf("%q: an empty field: %+v", tpl.Key, tpl)
		}
		if seen[tpl.Key] {
			t.Errorf("duplicate template key %q", tpl.Key)
		}
		seen[tpl.Key] = true
		if reg.Get(tpl.Mode) == nil {
			t.Errorf("%q: unknown mode %q", tpl.Key, tpl.Mode)
		}
		if !model.BudgetClass(tpl.BudgetClass).Valid() {
			t.Errorf("%q: invalid budget class %q", tpl.Key, tpl.BudgetClass)
		}
		if !strings.Contains(tpl.Prompt, "{{repo}}") {
			t.Errorf("%q: prompt should reference {{repo}}", tpl.Key)
		}
	}
}

func TestRoutineTemplatesEndpoint(t *testing.T) {
	h := newHarness(t, transportUnix)
	var out []RoutineTemplate
	h.call(http.MethodGet, "/api/v1/routine-templates", nil, &out, http.StatusOK)
	if len(out) != len(routineTemplates) {
		t.Fatalf("endpoint returned %d templates, want %d", len(out), len(routineTemplates))
	}
}
