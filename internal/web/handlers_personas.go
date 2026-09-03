package web

// Personas: the file-backed prompt library's HTTP surface, and the seam that
// composes a persona into a routine at Work creation. The library itself is
// git + Markdown (internal/core/prompts) — the daemon only reads it; these
// routes exist so the CLI and UI can list what is loaded and print exactly
// what an agent will read.

import (
	"net/http"
	"time"

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
	Body        string               `json:"body,omitempty"`
	Resolved    string               `json:"resolved,omitempty"`
	Composition *prompts.Composition `json:"composition,omitempty"`
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
