package controlplane

// POST /api/v1/notify/test journals a notify.test event the notify plugin
// turns into a real desktop toast whose click routes back into the UI. This
// backs the "Send test toast" buttons: the whole chain — journal, plugin,
// notify-send, the omarchy-exec-argv click hint — is exercised, not simulated.

import (
	"net/http"
	"strings"

	"forge/internal/store"
)

// notifyTestRequest optionally names the UI path the toast's click should
// open; the default is the Human queue the button lives beside.
type notifyTestRequest struct {
	Path string `json:"path"`
}

func (s *Server) notifyTest(r *http.Request) (int, any, error) {
	ctx := r.Context()
	req := notifyTestRequest{Path: "/attention"}
	if r.ContentLength != 0 {
		if err := decodeJSON(r, &req); err != nil {
			return 0, nil, err
		}
		if req.Path == "" {
			req.Path = "/attention"
		}
	}
	if !strings.HasPrefix(req.Path, "/") || strings.HasPrefix(req.Path, "//") {
		return 0, nil, badRequest("path must be a UI path starting with /")
	}
	if err := s.store.Write(ctx, func(tx *store.Tx) error {
		return tx.Journal(ctx, "notify.test", store.EntityDaemon, "notify", map[string]any{"path": req.Path})
	}); err != nil {
		return 0, nil, err
	}
	s.log.InfoContext(ctx, "notify test requested", "path", req.Path)
	return http.StatusAccepted, map[string]any{"queued": true, "path": req.Path}, nil
}
