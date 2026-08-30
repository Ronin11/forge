package controlplane

import (
	"net/http"
	"strconv"
	"time"

	"forge/internal/stats"
)

// defaultStatsWindow is the stats and retro window when ?since is absent —
// one week, matching the tools' default.
const defaultStatsWindow = 7 * 24 * time.Hour

// statsRoutes serves DESIGN.md §9.4: the per-routine aggregation and the
// retro data pack, both windowed by ?since.
func (s *Server) statsRoutes(m *http.ServeMux) {
	m.HandleFunc("GET /api/v1/stats", s.handle(s.statsReport))
	m.HandleFunc("GET /api/v1/retro", s.handle(s.retroPack))
}

// statsEnvelope versions the stats body; the retro pack carries its own
// schema_version.
type statsEnvelope struct {
	SchemaVersion int           `json:"schema_version"`
	Report        *stats.Report `json:"report"`
}

func (s *Server) statsReport(r *http.Request) (int, any, error) {
	q, err := s.statsQuery(r)
	if err != nil {
		return 0, nil, err
	}
	report, err := stats.Load(r.Context(), s.store, q)
	if err != nil {
		return 0, nil, err
	}
	return http.StatusOK, statsEnvelope{SchemaVersion: 1, Report: report}, nil
}

func (s *Server) retroPack(r *http.Request) (int, any, error) {
	q, err := s.statsQuery(r)
	if err != nil {
		return 0, nil, err
	}
	pack, err := stats.LoadRetroPack(r.Context(), s.store, q)
	if err != nil {
		return 0, nil, err
	}
	return http.StatusOK, pack, nil
}

// statsQuery builds the window ending now from ?since and the optional
// dimension filters both endpoints share.
func (s *Server) statsQuery(r *http.Request) (stats.Query, error) {
	window, err := parseSince(r.URL.Query().Get("since"))
	if err != nil {
		return stats.Query{}, err
	}
	until := s.now().UTC()
	v := r.URL.Query()
	return stats.Query{
		Since: until.Add(-window), Until: until,
		Routine: v.Get("routine"), Repository: v.Get("repository"), Project: v.Get("project"), Mode: v.Get("mode"),
	}, nil
}

// parseSince reads ?since=: a positive integer with an h (hours) or d (days)
// suffix — "24h", "1d", "7d". Empty means the one-week default.
func parseSince(raw string) (time.Duration, error) {
	if raw == "" {
		return defaultStatsWindow, nil
	}
	if len(raw) < 2 {
		return 0, badRequest("since %q: want <N>h or <N>d", raw)
	}
	n, err := strconv.Atoi(raw[:len(raw)-1])
	if err != nil || n <= 0 {
		return 0, badRequest("since %q: want <N>h or <N>d", raw)
	}
	switch raw[len(raw)-1] {
	case 'h':
		return time.Duration(n) * time.Hour, nil
	case 'd':
		return time.Duration(n) * 24 * time.Hour, nil
	}
	return 0, badRequest("since %q: want <N>h or <N>d", raw)
}
