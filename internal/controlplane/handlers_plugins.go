package controlplane

import "net/http"

// pluginRoutes registers the plugin surface (DESIGN.md §17): the journal SSE
// stream for events plugins, POST /api/v1/plugins/{name}/ack, and the
// list/install/enable/disable endpoints behind `forge plugin`. Stub: M7
// fills it.
func (s *Server) pluginRoutes(m *http.ServeMux) {
	_ = m
}
