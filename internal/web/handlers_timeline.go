package web

import (
	"net/http"
	"strconv"
	"time"

	"forge/internal/core/store"
)

// timelineRoutes serves the dashboard's live wall-clock timeline: every attempt
// overlapping a trailing window (default one hour), grouped and drawn client
// side. Served on both listeners like the other operator GETs.
func (s *Server) timelineRoutes(m *http.ServeMux) {
	m.HandleFunc("GET /api/v1/timeline", s.handle(s.timeline))
}

// Window bounds for the timeline: the default when ?window is absent and the
// clamp a client cannot escape.
const (
	timelineDefaultWindow = time.Hour
	timelineMinWindow     = 5 * time.Minute
	timelineMaxWindow     = 24 * time.Hour
)

// timelineResponse is GET /api/v1/timeline's wire shape: the window it covers
// and the attempts in it (an empty list, never null).
type timelineResponse struct {
	Now           time.Time            `json:"now"`
	Since         time.Time            `json:"since"`
	WindowSeconds int                  `json:"window_seconds"`
	Items         []store.TimelineItem `json:"items"`
}

func (s *Server) timeline(r *http.Request) (int, any, error) {
	window, err := parseWindow(r.URL.Query().Get("window"))
	if err != nil {
		return 0, nil, err
	}
	now := s.now().UTC()
	since := now.Add(-window)
	items, err := s.store.TimelineItems(r.Context(), since)
	if err != nil {
		return 0, nil, err
	}
	if items == nil {
		items = []store.TimelineItem{}
	}
	return http.StatusOK, timelineResponse{
		Now:           now,
		Since:         since,
		WindowSeconds: int(window / time.Second),
		Items:         items,
	}, nil
}

// parseWindow reads ?window=: a positive integer with an m, h, or d suffix —
// "15m", "1h", "6h". Empty means the one-hour default; the result is clamped to
// [timelineMinWindow, timelineMaxWindow]; anything else is a 400.
func parseWindow(raw string) (time.Duration, error) {
	if raw == "" {
		return timelineDefaultWindow, nil
	}
	if len(raw) < 2 {
		return 0, badRequest("window %q: want <N>m, <N>h, or <N>d", raw)
	}
	n, err := strconv.Atoi(raw[:len(raw)-1])
	if err != nil || n <= 0 {
		return 0, badRequest("window %q: want <N>m, <N>h, or <N>d", raw)
	}
	var d time.Duration
	switch raw[len(raw)-1] {
	case 'm':
		d = time.Duration(n) * time.Minute
	case 'h':
		d = time.Duration(n) * time.Hour
	case 'd':
		d = time.Duration(n) * 24 * time.Hour
	default:
		return 0, badRequest("window %q: want <N>m, <N>h, or <N>d", raw)
	}
	if d < timelineMinWindow {
		d = timelineMinWindow
	}
	if d > timelineMaxWindow {
		d = timelineMaxWindow
	}
	return d, nil
}
