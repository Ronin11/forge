package web

// Personas: the file-backed prompt library's HTTP surface, and the seam that
// composes a persona into a routine at Work creation. The library itself is
// git + Markdown (internal/core/prompts) — the daemon only reads it; these
// routes exist so the CLI and UI can list what is loaded and print exactly
// what an agent will read.

import (
	"encoding/json"
	"net/http"
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
}

func (s *Server) listPersonas(r *http.Request) (int, any, error) {
	lib := s.promptLibrary()
	if lib == nil {
		return 0, nil, badRequest("this process has no prompts library")
	}
	out := personasList{Commit: lib.Commit, Dirty: lib.Dirty, LoadedAt: lib.LoadedAt, Dir: lib.Dir, Rows: []personaRow{}}
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
	Resolved    string               `json:"resolved,omitempty"`
	Composition *prompts.Composition `json:"composition,omitempty"`
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
	return http.StatusOK, out, nil
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
	rt := *saved
	out := routinePreview{Mode: rt.Mode, Persona: rt.Persona}
	if rt.Persona != "" {
		comp, err := s.composePersona(&rt)
		if err != nil {
			return 0, nil, err
		}
		out.Composition = comp
	}
	rt.Prompt = injectObjective(rt.Prompt, r.URL.Query().Get("objective"))
	out.Model = rt.Model
	repo := r.URL.Query().Get("repo")
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
	return http.StatusOK, out, nil
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
