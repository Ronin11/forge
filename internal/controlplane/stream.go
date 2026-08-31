package controlplane

import "net/http"

// streamRoutes registers the SSE endpoints (DESIGN.md §13: GET
// /api/v1/work/{id}/stream, GET /api/v1/journal). Stub: M6 fills it.
func (s *Server) streamRoutes(m *http.ServeMux) {
	_ = m
}
