package web

import (
	"context"
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

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
	current := lib
	h.srv.prompts = func() *prompts.Library { return current }
	h.srv.promptsReload = func() error {
		next, err := prompts.Load(dir)
		if err != nil {
			return err
		}
		current = next
		return nil
	}
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

// The preview endpoint returns the byte-exact rendered prompt: persona
// composition, {{objective}} injection, {{repo}} substitution, and the
// assembly context — without creating any Work.
func TestRoutinePreview(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{
		"personas/reviewer.md": "---\nmodel: haiku\n---\nYou are the reviewer.\n\n## mode: run\nRun-mode teaching.",
	})
	rt := store.Routine{Name: "previewable", Mode: "run", Prompt: "Review {{repo}}: {{objective}}", Persona: "reviewer",
		Repositories: []string{"equitizr"}, TimeoutSeconds: 300}
	h.call(http.MethodPost, "/api/v1/routines", rt, nil, http.StatusCreated)

	var out struct {
		Prompt      string `json:"prompt"`
		Model       string `json:"model"`
		Composition *struct {
			Fragments []struct{ Name string } `json:"fragments"`
		} `json:"composition"`
	}
	h.call(http.MethodGet, "/api/v1/routines/previewable/preview?objective=check+the+gauges", nil, &out, http.StatusOK)
	for _, want := range []string{
		"You are the reviewer.",
		"Run-mode teaching.",
		"Review equitizr: check the gauges", // {{repo}} + {{objective}} both substituted
		"YOUR TASK",
		"repository equitizr",
	} {
		if !strings.Contains(out.Prompt, want) {
			t.Errorf("preview missing %q", want)
		}
	}
	if out.Model != "haiku" || out.Composition == nil || len(out.Composition.Fragments) != 1 {
		t.Errorf("model=%q composition=%+v", out.Model, out.Composition)
	}
	// No Work was created.
	works, err := h.st.OpenWork(context.Background())
	if err != nil || len(works) != 0 {
		t.Errorf("preview created work: %v, %v", works, err)
	}
}

// PUT edits an existing file: a good edit persists, hot-reloads, and serves
// the new composition; an edit that breaks the library is reverted and 400s
// with the loader's reason. The raw file — frontmatter and mode sections
// included — is what round-trips.
func TestPromptFragmentEdit(t *testing.T) {
	h := newHarness(t, transportUnix)
	lib := h.withPrompts(map[string]string{
		"personas/reviewer.md":   "---\nmodel: haiku\n---\nOld identity.\n{{> standards}}\n\n## mode: review\nOld teaching.",
		"fragments/standards.md": "Old standards.",
	})

	// The GET serves the raw file for the editor.
	var detail struct {
		Raw  string `json:"raw"`
		Body string `json:"body"`
	}
	h.call(http.MethodGet, "/api/v1/personas", nil, nil, http.StatusOK)
	h.call(http.MethodGet, "/api/v1/prompts/personas-check", nil, nil, http.StatusBadRequest)
	h.call(http.MethodGet, "/api/v1/prompts/reviewer", nil, &detail, http.StatusOK)
	if !strings.Contains(detail.Raw, "model: haiku") || !strings.Contains(detail.Raw, "## mode: review") {
		t.Fatalf("raw is not the whole file: %q", detail.Raw)
	}
	if strings.Contains(detail.Body, "## mode:") {
		t.Fatalf("body should be the stripped core: %q", detail.Body)
	}

	// A good edit lands, commits nothing here (no git in the temp tree — best
	// effort), and composes immediately via the reload seam.
	newContent := "---\nmodel: haiku\n---\nNew identity.\n{{> standards}}\n\n## mode: review\nNew teaching."
	var after struct {
		Raw string `json:"raw"`
	}
	h.call(http.MethodPut, "/api/v1/prompts/reviewer", map[string]string{"content": newContent}, &after, http.StatusOK)
	if !strings.Contains(after.Raw, "New identity.") {
		t.Fatalf("edit not served back: %q", after.Raw)
	}
	onDisk, err := os.ReadFile(filepath.Join(lib.Dir, "personas", "reviewer.md"))
	if err != nil || string(onDisk) != newContent {
		t.Fatalf("edit not on disk: %q, %v", onDisk, err)
	}
	var resolved struct {
		Resolved string `json:"resolved"`
	}
	h.call(http.MethodGet, "/api/v1/prompts/reviewer?resolved=1&mode=review", nil, &resolved, http.StatusOK)
	if !strings.Contains(resolved.Resolved, "New teaching.") {
		t.Fatalf("reload did not take: %q", resolved.Resolved)
	}

	// A breaking edit (unknown include) is refused and the file reverted.
	status, body := h.do(http.MethodPut, "/api/v1/prompts/standards", map[string]string{"content": "{{> ghost}}"}, nil, "")
	if status != http.StatusBadRequest || !strings.Contains(string(body), "not found") {
		t.Fatalf("breaking edit = %d %s", status, body)
	}
	onDisk, err = os.ReadFile(filepath.Join(lib.Dir, "fragments", "standards.md"))
	if err != nil || string(onDisk) != "Old standards." {
		t.Fatalf("breaking edit not reverted: %q, %v", onDisk, err)
	}

	// Only existing files: no create-by-PUT.
	if status, _ := h.do(http.MethodPut, "/api/v1/prompts/brand-new", map[string]string{"content": "x"}, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("create by PUT = %d", status)
	}
}

// ?test=1 runs a persona through the full assembly with a synthetic routine.
func TestPersonaTestPreview(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{
		"personas/reviewer.md": "---\nmodel: haiku\n---\nYou are the reviewer.\n\n## mode: run\nRun teaching.",
	})
	var out struct {
		Test *struct {
			Prompt string `json:"prompt"`
			Model  string `json:"model"`
		} `json:"test"`
	}
	h.call(http.MethodGet, "/api/v1/prompts/reviewer?test=1&mode=run&task=Fix+{{repo}}:+{{objective}}&objective=the+gauges&repo=equitizr", nil, &out, http.StatusOK)
	if out.Test == nil {
		t.Fatal("no test preview")
	}
	for _, want := range []string{"You are the reviewer.", "Run teaching.", "Fix equitizr: the gauges", "YOUR TASK"} {
		if !strings.Contains(out.Test.Prompt, want) {
			t.Errorf("test preview missing %q", want)
		}
	}
	if out.Test.Model != "haiku" {
		t.Errorf("model = %q", out.Test.Model)
	}
}

// POST /api/v1/prompt-test runs the composed prompt through the model seam:
// the persona path composes before calling, the routine path uses the saved
// binding, the model override wins, and a process without model access or an
// unknown alias refuses cleanly.
func TestPromptTestRun(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{
		"personas/reviewer.md": "---\nmodel: haiku\n---\nYou are the reviewer.\n\n## mode: run\nRun teaching.",
	})

	// No model seam: refused before anything runs.
	if status, body := h.do(http.MethodPost, "/api/v1/prompt-test", map[string]string{"persona": "reviewer", "task": "t"}, nil, ""); status != http.StatusBadRequest || !strings.Contains(string(body), "model access") {
		t.Fatalf("no seam = %d %s", status, body)
	}

	var gotUser, gotModel string
	h.srv.modelCall = func(ctx context.Context, system, user, model string) (string, error) {
		gotUser, gotModel = user, model
		return "MODEL SAYS HI", nil
	}

	var out struct {
		Output    string `json:"output"`
		Model     string `json:"model"`
		Prompt    string `json:"prompt"`
		ElapsedMS *int64 `json:"elapsed_ms"`
	}
	h.call(http.MethodPost, "/api/v1/prompt-test", map[string]string{
		"persona": "reviewer", "mode": "run", "task": "Fix {{repo}}: {{objective}}",
		"objective": "the gauges", "repo": "equitizr",
	}, &out, http.StatusOK)
	if out.Output != "MODEL SAYS HI" || out.Model != "haiku" || gotModel != "haiku" {
		t.Fatalf("out = %+v, gotModel = %q", out, gotModel)
	}
	for _, want := range []string{"You are the reviewer.", "Run teaching.", "Fix equitizr: the gauges"} {
		if !strings.Contains(gotUser, want) {
			t.Errorf("model call prompt missing %q", want)
		}
	}
	if out.ElapsedMS == nil {
		t.Error("no elapsed_ms")
	}

	// Model override beats the persona default.
	h.call(http.MethodPost, "/api/v1/prompt-test", map[string]string{"persona": "reviewer", "task": "t", "model": "sonnet"}, &out, http.StatusOK)
	if gotModel != "sonnet" || out.Model != "sonnet" {
		t.Errorf("override: gotModel=%q out.Model=%q", gotModel, out.Model)
	}

	// The routine path uses the saved binding.
	rt := store.Routine{Name: "runnable", Mode: "run", Prompt: "Routine task on {{repo}}", Persona: "reviewer", Repositories: []string{"equitizr"}, TimeoutSeconds: 300}
	h.call(http.MethodPost, "/api/v1/routines", rt, nil, http.StatusCreated)
	h.call(http.MethodPost, "/api/v1/prompt-test", map[string]string{"routine": "runnable"}, &out, http.StatusOK)
	if !strings.Contains(gotUser, "Routine task on equitizr") || !strings.Contains(gotUser, "You are the reviewer.") {
		t.Errorf("routine test prompt = %q", gotUser)
	}

	// Refusals: unknown alias, and neither routine nor persona.
	if status, body := h.do(http.MethodPost, "/api/v1/prompt-test", map[string]string{"persona": "reviewer", "task": "t", "model": "bogus"}, nil, ""); status != http.StatusBadRequest || !strings.Contains(string(body), "unknown model alias") {
		t.Fatalf("bad alias = %d %s", status, body)
	}
	if status, _ := h.do(http.MethodPost, "/api/v1/prompt-test", map[string]string{"task": "t"}, nil, ""); status != http.StatusBadRequest {
		t.Fatalf("no subject = %d", status)
	}
}

// Every test run is recorded against its subject with inputs, manifest, and
// output — the page's "last ran with these inputs" memory — trimmed to the
// most recent per subject.
func TestPromptTestHistory(t *testing.T) {
	h := newHarness(t, transportUnix)
	h.register(testWorkerID)
	h.withPrompts(map[string]string{"personas/reviewer.md": "---\nmodel: haiku\n---\nYou are the reviewer."})
	h.srv.modelCall = func(ctx context.Context, system, user, model string) (string, error) {
		return "OUTPUT for " + model, nil
	}

	h.call(http.MethodPost, "/api/v1/prompt-test", map[string]string{"persona": "reviewer", "task": "first try", "objective": "obj-1"}, nil, http.StatusOK)
	h.clock.Advance(time.Second) // same-instant rows would tie-break on random ids
	h.call(http.MethodPost, "/api/v1/prompt-test", map[string]string{"persona": "reviewer", "task": "second try", "model": "sonnet"}, nil, http.StatusOK)

	var tests []store.PromptTest
	h.call(http.MethodGet, "/api/v1/prompt-tests?subject=persona:reviewer", nil, &tests, http.StatusOK)
	if len(tests) != 2 {
		t.Fatalf("tests = %d", len(tests))
	}
	latest, prev := tests[0], tests[1]
	if latest.Task != "second try" || latest.Model != "sonnet" || latest.Output != "OUTPUT for sonnet" {
		t.Errorf("latest = %+v", latest)
	}
	if prev.Task != "first try" || prev.Objective != "obj-1" || prev.Model != "haiku" {
		t.Errorf("prev = %+v", prev)
	}
	if !strings.Contains(latest.Prompt, "You are the reviewer.") || len(latest.Composition) == 0 {
		t.Errorf("latest missing prompt/manifest: %+v", latest)
	}
	// Subjects are separate: nothing recorded under a routine subject.
	h.call(http.MethodGet, "/api/v1/prompt-tests?subject=routine:reviewer", nil, &tests, http.StatusOK)
	if len(tests) != 0 {
		t.Errorf("cross-subject leak: %d", len(tests))
	}
	if status, _ := h.do(http.MethodGet, "/api/v1/prompt-tests", nil, nil, ""); status != http.StatusBadRequest {
		t.Errorf("no subject = %d", status)
	}

	// The per-subject trim keeps the scratchpad bounded.
	for i := 0; i < 25; i++ {
		h.call(http.MethodPost, "/api/v1/prompt-test", map[string]string{"persona": "reviewer", "task": "spam"}, nil, http.StatusOK)
	}
	h.call(http.MethodGet, "/api/v1/prompt-tests?subject=persona:reviewer", nil, &tests, http.StatusOK)
	if len(tests) != 20 {
		t.Errorf("trim: %d rows, want 20", len(tests))
	}
}
