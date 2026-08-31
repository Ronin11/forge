package controlplane

import "net/http"

// doctorRoutes registers GET /api/v1/doctor (DESIGN.md: the daemon-side
// checks doctor asks for when reachable). Stub: M6 fills it.
func (s *Server) doctorRoutes(m *http.ServeMux) {
	_ = m
}
