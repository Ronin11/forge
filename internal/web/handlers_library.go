package web

// The library discovery surface: one search implementation (Library.Search +
// workflow rows merged with the same scoring) behind GET /api/v1/library/
// search, serving the Directives page filter, `forge directives search`, and
// the forge_library agent tool; plus POST /api/v1/script-test, the operator's
// one-click sandbox run of a library script.

import (
	"encoding/json"
	"net/http"
	"strconv"
	"strings"

	"forge/internal/core/directives"
	"forge/internal/core/flow"
)

// librarySearch is GET /api/v1/library/search?q=&kind=&limit=.
func (s *Server) librarySearch(r *http.Request) (int, any, error) {
	lib := s.promptLibrary()
	if lib == nil {
		return 0, nil, badRequest("this process has no library")
	}
	q := r.URL.Query()
	kinds := map[string]bool{}
	for _, k := range q["kind"] {
		for _, one := range strings.Split(k, ",") {
			if one = strings.TrimSpace(one); one != "" {
				kinds[one] = true
			}
		}
	}
	limit := 50
	if raw := q.Get("limit"); raw != "" {
		n, err := strconv.Atoi(raw)
		if err != nil || n < 1 || n > 200 {
			return 0, nil, badRequest("limit %q: want 1..200", raw)
		}
		limit = n
	}
	hits := lib.Search(q.Get("q"), kinds, 0)
	// Workflows and scratch scripts are SQLite rows; merge them with
	// identical scoring.
	terms := strings.Fields(strings.ToLower(q.Get("q")))
	if len(kinds) == 0 || kinds["workflow"] {
		wfs, err := s.store.ListWorkflows(r.Context(), false)
		if err != nil {
			return 0, nil, err
		}
		for _, wf := range wfs {
			if score, ok := directives.ScoreTerms(terms, wf.Name, wf.Description, ""); ok {
				hits = append(hits, directives.SearchHit{Name: wf.Name, Kind: "workflow", Description: wf.Description, Tool: wf.Tool, Score: score})
			}
		}
	}
	if len(kinds) == 0 || kinds["scratch"] {
		scratch, err := s.store.ListScratch(r.Context())
		if err != nil {
			return 0, nil, err
		}
		for _, sc := range scratch {
			if score, ok := directives.ScoreTerms(terms, sc.Name, sc.Description, sc.Source); ok {
				hits = append(hits, directives.SearchHit{Name: sc.Name, Kind: "scratch", Description: sc.Description, InputSchema: sc.InputSchema, Score: score})
			}
		}
	}
	directives.SortHits(hits)
	if len(hits) > limit {
		hits = hits[:limit]
	}
	if hits == nil {
		hits = []directives.SearchHit{}
	}
	return http.StatusOK, map[string]any{"hits": hits}, nil
}

// scriptTestRequest is POST /api/v1/script-test: run a library script (or
// inline source) once in the sandbox with the given input as input.params —
// the directive-test pattern for pure compute. Operator surface.
type scriptTestRequest struct {
	Name   string          `json:"name"`
	Source string          `json:"source"`
	Input  json.RawMessage `json:"input"`
}

func (s *Server) scriptTest(r *http.Request) (int, any, error) {
	if s.Draining() {
		return 0, nil, errDraining
	}
	var req scriptTestRequest
	if err := decodeJSON(r, &req); err != nil {
		return 0, nil, err
	}
	source, timeoutMS := req.Source, 0
	var interpreter []string
	path := ""
	if req.Name != "" {
		lib := s.promptLibrary()
		if lib == nil {
			return 0, nil, badRequest("this process has no library")
		}
		f := lib.Script(req.Name)
		if f == nil {
			return 0, nil, badRequest("script %q is not in the library (scripts/)", req.Name)
		}
		source, interpreter, path, timeoutMS = f.Body, f.Interpreter, f.Path, f.TimeoutMS
	}
	if strings.TrimSpace(source) == "" {
		return 0, nil, badRequest("name a library script or pass source")
	}
	input := flow.ScriptInput{}
	if len(req.Input) > 0 {
		var params any
		if err := json.Unmarshal(req.Input, &params); err != nil {
			return 0, nil, badRequest("input: %v", err)
		}
		input.Params = params
	}
	start := s.now()
	out, err := flow.RunAny(interpreter, path, source, input, timeoutMS)
	elapsed := s.now().Sub(start).Milliseconds()
	if err != nil {
		return http.StatusOK, map[string]any{"error": err.Error(), "elapsed_ms": elapsed}, nil
	}
	return http.StatusOK, map[string]any{"output": out, "elapsed_ms": elapsed}, nil
}
