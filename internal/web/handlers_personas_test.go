package web

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"forge/internal/core/prompts"
	"forge/internal/core/store"
)

// withPrompts hands the harness a file-backed library, the way the daemon
// injects its last-good load.
func (h *harness) withPrompts(files map[string]string) *prompts.Library {
	h.t.Helper()
	dir := h.t.TempDir()
	for name, content := range files {
		path := filepath.Join(dir, name)
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			h.t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
			h.t.Fatal(err)
		}
	}
	lib, err := prompts.Load(dir)
	if err != nil {
		h.t.Fatal(err)
	}
	h.srv.prompts = func() *prompts.Library { return lib }
	return lib
}

// A routine naming a persona gets the resolved text composed ahead of its
// prompt in the frozen snapshot, the persona's default model where its own is
// empty, and a composition manifest on the Work.
func TestPersonaComposesIntoWork(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{
		"personas/reviewer.md":   "---\nmodel: haiku\n---\nYou are the reviewer.\n{{> standards}}\n\n## mode: run\nRun-mode teaching.",
		"fragments/standards.md": "Be honest, not flattering.",
	})

	rt := store.Routine{Name: "audited", Mode: "run", Prompt: "task: {{objective}}", Persona: "reviewer",
		Repositories: []string{"equitizr"}, TimeoutSeconds: 300} // no model: the persona's default applies
	h.call(http.MethodPost, "/api/v1/routines", rt, &rt, http.StatusCreated)

	var out workCreated
	h.call(http.MethodPost, "/api/v1/routines/audited/run", map[string]string{"objective": "check the gauges"}, &out, http.StatusCreated)
	snap := string(out.Work.Snapshot)
	for _, want := range []string{
		"You are the reviewer.",
		"Be honest, not flattering.",
		"Run-mode teaching.",     // mode section matched the routine's mode
		"task: check the gauges", // objective injected after composition
		`"model":"haiku"`,        // persona default model
		`"persona":"reviewer"`,
	} {
		if !strings.Contains(snap, want) {
			t.Errorf("snapshot missing %q\n%s", want, snap)
		}
	}
	if out.Work.Persona != "reviewer" {
		t.Errorf("work.persona = %q", out.Work.Persona)
	}
	var comp prompts.Composition
	if err := json.Unmarshal(out.Work.Composition, &comp); err != nil {
		t.Fatalf("composition: %v (%s)", err, out.Work.Composition)
	}
	if comp.Persona != "reviewer" || comp.Mode != "run" || len(comp.Fragments) != 2 {
		t.Errorf("composition = %+v", comp)
	}
	// The persona text is ahead of the task text.
	if strings.Index(snap, "You are the reviewer.") > strings.Index(snap, "task: check the gauges") {
		t.Error("persona text is not ahead of the routine prompt")
	}
}

// Definition-time refusals: an unknown persona 400s at routine save when the
// library is loaded, and a routine with neither model nor persona 400s.
func TestPersonaValidation(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.withPrompts(map[string]string{"personas/real.md": "I exist."})

	ghost := store.Routine{Name: "g", Mode: "run", Prompt: "p", Persona: "ghost", Model: "haiku", Repositories: []string{"equitizr"}, TimeoutSeconds: 300}
	if status, body := h.do(http.MethodPost, "/api/v1/routines", ghost, nil, ""); status != http.StatusBadRequest || !strings.Contains(string(body), "not in the prompts library") {
		t.Fatalf("ghost persona = %d %s", status, body)
	}
	modeless := store.Routine{Name: "m", Mode: "run", Prompt: "p", Repositories: []string{"equitizr"}, TimeoutSeconds: 300}
	if status, body := h.do(http.MethodPost, "/api/v1/routines", modeless, nil, ""); status != http.StatusBadRequest || !strings.Contains(string(body), "model is required") {
		t.Fatalf("no model no persona = %d %s", status, body)
	}
	// A persona without a default model still needs the routine to name one at
	// run time; saving is allowed (the persona may gain a model later), but
	// running fails clearly.
	nomodel := store.Routine{Name: "n", Mode: "run", Prompt: "p", Persona: "real", Repositories: []string{"equitizr"}, TimeoutSeconds: 300}
	h.call(http.MethodPost, "/api/v1/routines", nomodel, nil, http.StatusCreated)
	if status, body := h.do(http.MethodPost, "/api/v1/routines/n/run", nil, nil, ""); status != http.StatusBadRequest || !strings.Contains(string(body), "model") {
		t.Fatalf("run without any model = %d %s", status, body)
	}
}

// The API lists the library and resolves a persona; a per-run persona
// override works on ad-hoc tasks too.
func TestPersonaAPIAndAdHocOverride(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{
		"personas/triager.md":  "Triage fast.\n{{> brevity}}",
		"fragments/brevity.md": "Answer in one line.",
	})

	var list struct {
		Dirty bool `json:"dirty"`
		Rows  []struct {
			Name    string `json:"name"`
			Persona bool   `json:"persona"`
		} `json:"fragments"`
	}
	h.call(http.MethodGet, "/api/v1/personas", nil, &list, http.StatusOK)
	if len(list.Rows) != 2 || !list.Dirty {
		t.Fatalf("list = %+v", list)
	}

	var detail struct {
		Resolved string `json:"resolved"`
	}
	h.call(http.MethodGet, "/api/v1/personas/triager?resolved=1", nil, &detail, http.StatusOK)
	if !strings.Contains(detail.Resolved, "Answer in one line.") {
		t.Errorf("resolved = %q", detail.Resolved)
	}
	if status, _ := h.do(http.MethodGet, "/api/v1/personas/brevity", nil, nil, ""); status != http.StatusBadRequest {
		t.Errorf("fragment served as persona: %d", status)
	}

	var out workCreated
	h.call(http.MethodPost, "/api/v1/tasks", map[string]any{"prompt": "sort my inbox", "repositories": []string{"equitizr"}, "persona": "triager"}, &out, http.StatusCreated)
	if !strings.Contains(string(out.Work.Snapshot), "Triage fast.") || out.Work.Persona != "triager" {
		t.Errorf("ad-hoc persona not composed: %s", out.Work.Snapshot)
	}
}
