package web

// Personas: the file-backed prompt library's HTTP surface, and the seam that
// composes a persona into a routine at Work creation. The library itself is
// git + Markdown (internal/core/prompts) — the daemon only reads it; these
// routes exist so the CLI and UI can list what is loaded and print exactly
// what an agent will read.

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"sort"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/prompts"
	"forge/internal/core/store"
)

// composePersona resolves rt.Persona for rt.Mode and rewrites rt in place:
// the resolved text ahead of the prompt (so the snapshot and prompt_hash
// carry the exact bytes) and the persona's default model where the routine
// left it empty. Called inside createWorkTx; failures surface as 400s — and,
// for workflow nodes, as engine materialization failures that fail the node.
func (s *Server) composePersona(rt *store.Routine) (*prompts.Composition, error) {
	lib := s.promptLibrary()
	if lib == nil {
		return nil, badRequest("routine %s names persona %q but this process has no prompts library", rt.Name, rt.Persona)
	}
	text, comp, err := lib.Resolve(rt.Persona, rt.Mode)
	if err != nil {
		return nil, badRequest("%v", err)
	}
	if rt.Model == "" {
		rt.Model = lib.Persona(rt.Persona).Model
	}
	if text != "" {
		rt.Prompt = text + "\n\n" + rt.Prompt
	}
	return &comp, nil
}

func (s *Server) promptLibrary() *prompts.Library {
	if s.prompts == nil {
		return nil
	}
	return s.prompts()
}

// personaRow is one entry of GET /api/v1/personas.
type personaRow struct {
	Name    string   `json:"name"`
	Model   string   `json:"model,omitempty"`
	Modes   []string `json:"modes,omitempty"`
	Hash    string   `json:"hash"`
	Persona bool     `json:"persona"`
}

// personasList is the response envelope: the library's provenance plus every
// fragment (personas flagged).
type personasList struct {
	Commit   string       `json:"commit,omitempty"`
	Dirty    bool         `json:"dirty"`
	LoadedAt time.Time    `json:"loaded_at"`
	Dir      string       `json:"dir"`
	Rows     []personaRow `json:"fragments"`
	// Models are the daemon's model aliases, for the page's test-run picker.
	Models []string `json:"models,omitempty"`
}

func (s *Server) listPersonas(r *http.Request) (int, any, error) {
	lib := s.promptLibrary()
	if lib == nil {
		return 0, nil, badRequest("this process has no prompts library")
	}
	out := personasList{Commit: lib.Commit, Dirty: lib.Dirty, LoadedAt: lib.LoadedAt, Dir: lib.Dir, Rows: []personaRow{}, Models: s.modelAliases}
	for _, f := range lib.Fragments() {
		row := personaRow{Name: f.Name, Model: f.Model, Hash: f.Hash, Persona: f.Persona}
		for mode := range f.Modes {
			row.Modes = append(row.Modes, mode)
		}
		out.Rows = append(out.Rows, row)
	}
	return http.StatusOK, out, nil
}

// personaDetail is GET /api/v1/personas/{name}: the raw body and, when
// ?resolved=1, the exact composed text an agent would read for ?mode=M.
type personaDetail struct {
	personaRow
	Path        string               `json:"path,omitempty"`
	Body        string               `json:"body,omitempty"`
	Raw         string               `json:"raw,omitempty"`
	Resolved    string               `json:"resolved,omitempty"`
	Composition *prompts.Composition `json:"composition,omitempty"`
	Test        *routinePreview      `json:"test,omitempty"`
}

// getPromptFragment is GET /api/v1/prompts/{name...}: any library file —
// fragment or persona, nested names included — with its raw body; personas
// resolve with ?resolved=1&mode=M like getPersona.
func (s *Server) getPromptFragment(r *http.Request) (int, any, error) {
	lib := s.promptLibrary()
	if lib == nil {
		return 0, nil, badRequest("this process has no prompts library")
	}
	name := r.PathValue("name")
	f := lib.Fragment(name)
	if f == nil {
		return 0, nil, badRequest("%q is not in the prompts library", name)
	}
	out := personaDetail{personaRow: personaRow{Name: f.Name, Model: f.Model, Hash: f.Hash, Persona: f.Persona}, Body: f.Body, Path: f.Path}
	// The raw file, exactly as on disk: the page's editor round-trips this,
	// never the split view (Body is the frontmatter- and mode-stripped core).
	if raw, err := os.ReadFile(f.Path); err == nil {
		out.Raw = string(raw)
	}
	for mode := range f.Modes {
		out.Modes = append(out.Modes, mode)
	}
	sort.Strings(out.Modes)
	if f.Persona && r.URL.Query().Get("resolved") == "1" {
		text, comp, err := lib.Resolve(name, r.URL.Query().Get("mode"))
		if err != nil {
			return 0, nil, badRequest("%v", err)
		}
		out.Resolved, out.Composition = text, &comp
	}
	// ?test=1 runs a persona through the full assembly path with a synthetic
	// routine — mode, task text, objective, and repo from the query — so the
	// page can answer "what would an agent wearing this persona read" without
	// a routine existing yet.
	if f.Persona && r.URL.Query().Get("test") == "1" {
		q := r.URL.Query()
		mode := q.Get("mode")
		if mode == "" {
			mode = "run"
		}
		rt := store.Routine{Name: "(persona test)", Mode: mode, Prompt: q.Get("task"), Persona: name}
		preview, err := s.renderPreview(r.Context(), rt, q.Get("objective"), q.Get("repo"))
		if err != nil {
			return 0, nil, err
		}
		out.Test = &preview
	}
	return http.StatusOK, out, nil
}

// putPromptFragment is PUT /api/v1/prompts/{name...}: the page's save path
// for an existing file. The write is validated by reloading the whole tree —
// an edit that breaks composition (unknown include, cycle, bad frontmatter)
// is reverted and refused, so the library on disk is never left broken — then
// committed (best-effort) and hot-reloaded so the page composes the new
// version immediately. Creating files stays with git and your editor.
func (s *Server) putPromptFragment(r *http.Request) (int, any, error) {
	if s.Draining() {
		return 0, nil, errDraining
	}
	lib := s.promptLibrary()
	if lib == nil {
		return 0, nil, badRequest("this process has no prompts library")
	}
	name := r.PathValue("name")
	f := lib.Fragment(name)
	if f == nil {
		return 0, nil, badRequest("%q is not in the prompts library; create new files in %s with your editor", name, lib.Dir)
	}
	var body struct {
		Content string `json:"content"`
	}
	if err := decodeJSON(r, &body); err != nil {
		return 0, nil, err
	}
	old, err := os.ReadFile(f.Path)
	if err != nil {
		return 0, nil, err
	}
	if err := os.WriteFile(f.Path, []byte(body.Content), 0o644); err != nil {
		return 0, nil, err
	}
	if _, err := prompts.Load(lib.Dir); err != nil {
		if rerr := os.WriteFile(f.Path, old, 0o644); rerr != nil {
			s.log.ErrorContext(r.Context(), "revert refused prompt edit", "path", f.Path, "error", rerr)
		}
		return 0, nil, badRequest("refused — the edit breaks the library: %v", err)
	}
	prompts.CommitEdit(lib.Dir, f.Path, "ui: edit "+name)
	if s.promptsReload != nil {
		if err := s.promptsReload(); err != nil {
			s.log.WarnContext(r.Context(), "prompts reload after edit", "error", err)
		}
	}
	s.log.InfoContext(r.Context(), "prompt fragment edited", "fragment", name)
	// Serve the updated detail from the fresh library.
	return s.getPromptFragment(r)
}

// routinePreview is GET /api/v1/routines/{name}/preview: the exact rendered
// prompt an attempt of this routine would receive — persona composition, the
// mode preamble and overlays, the autonomy block, {{objective}} and {{repo}}
// substitution, declared checks — without creating any Work. The one honest
// answer to "what will the agent actually read".
type routinePreview struct {
	Prompt      string               `json:"prompt"`
	Model       string               `json:"model"`
	Mode        string               `json:"mode"`
	Persona     string               `json:"persona,omitempty"`
	Repository  string               `json:"repository,omitempty"`
	Composition *prompts.Composition `json:"composition,omitempty"`
}

func (s *Server) previewRoutine(r *http.Request) (int, any, error) {
	ctx := r.Context()
	saved, err := s.store.GetRoutine(ctx, r.PathValue("name"))
	if err != nil {
		return 0, nil, err
	}
	out, err := s.renderPreview(ctx, *saved, r.URL.Query().Get("objective"), r.URL.Query().Get("repo"))
	if err != nil {
		return 0, nil, err
	}
	return http.StatusOK, out, nil
}

// renderPreview runs the real assembly path over a routine (saved or
// synthetic) without creating any Work: persona composition, {{objective}}
// injection, mode preamble and overlays, autonomy block, {{repo}}
// substitution, declared checks.
func (s *Server) renderPreview(ctx context.Context, rt store.Routine, objective, repo string) (routinePreview, error) {
	out := routinePreview{Mode: rt.Mode, Persona: rt.Persona}
	if rt.Persona != "" {
		comp, err := s.composePersona(&rt)
		if err != nil {
			return out, err
		}
		out.Composition = comp
	}
	rt.Prompt = injectObjective(rt.Prompt, objective)
	out.Model = rt.Model
	if repo == "" && len(rt.Repositories) > 0 {
		repo = rt.Repositories[0]
	}
	out.Repository = repo
	in := promptInput{Mode: rt.Mode, RoutinePrompt: rt.Prompt, Repository: repo, Home: s.home, AttemptID: "(preview)"}
	in.Autonomy = model.ResolveAutonomy("", rt.Autonomy, "", "", "")
	if s.modelInfo != nil {
		if info, ok := s.modelInfo(rt.Model); ok {
			in.ModelClass = info.Class
		}
	}
	if s.modes != nil {
		if m := s.modes.Get(rt.Mode); m != nil {
			in.ModePreamble = m.Preamble()
			in.Checkpoints = m.Checkpoints()
		}
	}
	if repos, err := s.store.Repositories(ctx); err == nil {
		for _, rep := range repos {
			if rep.Name != repo {
				continue
			}
			in.RepoPath = rep.Path
			if rep.ForgeToml != "" {
				var ft struct {
					Checks map[string][]string `json:"Checks"`
				}
				if json.Unmarshal([]byte(rep.ForgeToml), &ft) == nil {
					for name := range ft.Checks {
						in.DeclaredChecks = append(in.DeclaredChecks, name)
					}
					sort.Strings(in.DeclaredChecks)
				}
			}
		}
	}
	_, rendered := assemblePrompt(in)
	out.Prompt = rendered
	return out, nil
}

// promptTest is POST /api/v1/prompt-test: build the preview (a saved routine
// or a synthetic persona+task) and actually run it — one headless model call
// through the concierge's seam, with the model alias configurable — so
// "how does this prompt land" is answerable from the page. This is a prompt
// smoke, not an agent run: no worktree, no tools, no task; it costs one
// completion at the chosen model size.
type promptTestRequest struct {
	Routine   string `json:"routine"`
	Persona   string `json:"persona"`
	Mode      string `json:"mode"`
	Task      string `json:"task"`
	Objective string `json:"objective"`
	Repo      string `json:"repo"`
	Model     string `json:"model"` // alias override; empty = the effective model
}

type promptTestResponse struct {
	routinePreview
	Output    string `json:"output"`
	ElapsedMS int64  `json:"elapsed_ms"`
}

func (s *Server) promptTest(r *http.Request) (int, any, error) {
	ctx := r.Context()
	if s.Draining() {
		return 0, nil, errDraining
	}
	if s.modelCall == nil {
		return 0, nil, badRequest("test runs need the daemon's model access, which this process does not have")
	}
	var req promptTestRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	var rt store.Routine
	switch {
	case req.Routine != "":
		saved, err := s.store.GetRoutine(ctx, req.Routine)
		if err != nil {
			return 0, nil, err
		}
		rt = *saved
	case req.Persona != "":
		mode := req.Mode
		if mode == "" {
			mode = "run"
		}
		rt = store.Routine{Name: "(prompt test)", Mode: mode, Prompt: req.Task, Persona: req.Persona}
	default:
		return 0, nil, badRequest("name a routine or a persona to test")
	}
	preview, err := s.renderPreview(ctx, rt, req.Objective, req.Repo)
	if err != nil {
		return 0, nil, err
	}
	alias := req.Model
	if alias == "" {
		alias = preview.Model
	}
	if alias == "" {
		return 0, nil, badRequest("pick a model — neither the persona nor the request names one")
	}
	if _, ok := s.resolveModel(alias); !ok {
		return 0, nil, badRequest("unknown model alias %q", alias)
	}
	start := s.now()
	output, err := s.modelCall(ctx, "", preview.Prompt, alias)
	if err != nil {
		return 0, nil, fmt.Errorf("test run (%s): %w", alias, err)
	}
	preview.Model = alias
	s.log.InfoContext(ctx, "prompt test run", "routine", req.Routine, "persona", req.Persona, "model", alias, "prompt_bytes", len(preview.Prompt), "output_bytes", len(output))
	return http.StatusOK, promptTestResponse{routinePreview: preview, Output: output, ElapsedMS: s.now().Sub(start).Milliseconds()}, nil
}

func (s *Server) getPersona(r *http.Request) (int, any, error) {
	lib := s.promptLibrary()
	if lib == nil {
		return 0, nil, badRequest("this process has no prompts library")
	}
	name := r.PathValue("name")
	f := lib.Persona(name)
	if f == nil {
		return 0, nil, badRequest("persona %q is not in the library", name)
	}
	out := personaDetail{personaRow: personaRow{Name: f.Name, Model: f.Model, Hash: f.Hash, Persona: true}, Body: f.Body}
	for mode := range f.Modes {
		out.Modes = append(out.Modes, mode)
	}
	if r.URL.Query().Get("resolved") == "1" {
		text, comp, err := lib.Resolve(name, r.URL.Query().Get("mode"))
		if err != nil {
			return 0, nil, badRequest("%v", err)
		}
		out.Resolved, out.Composition = text, &comp
	}
	return http.StatusOK, out, nil
}
