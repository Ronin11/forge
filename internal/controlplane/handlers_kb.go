package controlplane

import (
	"fmt"
	"net/http"
	"os"

	"forge/internal/kb"
	"forge/internal/store"
)

// kbRoutes are the operator's index-backed kb endpoints: the CLI's search,
// backlinks, and links, the fact resolver `forge kb check` uses, and the
// reindex ping `forge kb new` sends so a fresh note is searchable at once.
func (s *Server) kbRoutes(m *http.ServeMux) {
	m.HandleFunc("GET /api/v1/kb/search", s.handle(s.kbSearch))
	m.HandleFunc("GET /api/v1/kb/backlinks", s.handle(s.kbBacklinks))
	m.HandleFunc("GET /api/v1/kb/links", s.handle(s.kbLinks))
	m.HandleFunc("GET /api/v1/kb/resolve-fact", s.handle(s.kbResolveFact))
	m.HandleFunc("POST /api/v1/kb/reindex", s.handle(s.kbReindex))
}

func (s *Server) kbSearch(r *http.Request) (int, any, error) {
	q := r.URL.Query().Get("q")
	if q == "" {
		return 0, nil, badRequest("q is required")
	}
	limit, err := listLimit(r)
	if err != nil {
		return 0, nil, err
	}
	notes, err := s.store.SearchKb(r.Context(), q, min(limit, 100))
	if err != nil {
		return 0, nil, err
	}
	if notes == nil {
		notes = []store.KbNote{}
	}
	return http.StatusOK, notes, nil
}

func (s *Server) kbBacklinks(r *http.Request) (int, any, error) {
	ref, err := kb.ParseRef(r.URL.Query().Get("ref"))
	if err != nil {
		return 0, nil, badRequest("%v", err)
	}
	links, err := s.store.KbBacklinks(r.Context(), ref)
	if err != nil {
		return 0, nil, err
	}
	if links == nil {
		links = []store.KbLink{}
	}
	return http.StatusOK, links, nil
}

func (s *Server) kbLinks(r *http.Request) (int, any, error) {
	id := r.URL.Query().Get("id")
	if id == "" {
		return 0, nil, badRequest("id is required")
	}
	links, err := s.store.KbLinks(r.Context(), id)
	if err != nil {
		return 0, nil, err
	}
	if links == nil {
		links = []store.KbLink{}
	}
	return http.StatusOK, links, nil
}

func (s *Server) kbResolveFact(r *http.Request) (int, any, error) {
	ref, err := kb.ParseRef(r.URL.Query().Get("ref"))
	if err != nil {
		return 0, nil, badRequest("%v", err)
	}
	exists, err := s.store.KbFactExists(r.Context(), ref)
	if err != nil {
		return 0, nil, err
	}
	return http.StatusOK, map[string]any{"schema_version": 1, "exists": exists}, nil
}

// kbReindex rescans the kb directory now rather than on the daemon's timer.
func (s *Server) kbReindex(r *http.Request) (int, any, error) {
	if s.kbDir == "" {
		return 0, nil, badRequest("this daemon has no kb directory configured")
	}
	if _, err := os.Stat(s.kbDir); os.IsNotExist(err) {
		return http.StatusOK, map[string]any{"schema_version": 1, "indexed": 0}, nil
	}
	notes, findings, err := kb.Scan(s.kbDir)
	if err != nil {
		return 0, nil, fmt.Errorf("scan kb: %w", err)
	}
	for _, f := range findings {
		s.log.WarnContext(r.Context(), "kb note skipped", "path", f.Path, "problem", f.Problem)
	}
	var indexed int
	err = s.store.Write(r.Context(), func(tx *store.Tx) error {
		var werr error
		indexed, werr = tx.ReindexKb(r.Context(), notes)
		return werr
	})
	if err != nil {
		return 0, nil, err
	}
	return http.StatusOK, map[string]any{"schema_version": 1, "indexed": indexed, "notes": len(notes)}, nil
}
