package controlplane

import (
	"net/http"
	"time"

	"forge/internal/doctor"
)

// doctorRoutes registers GET /api/v1/doctor: the daemon-side checks `forge
// doctor` asks for when the socket answers. The checks themselves live in
// internal/doctor; this handler only gathers their inputs.
func (s *Server) doctorRoutes(m *http.ServeMux) {
	m.HandleFunc("GET /api/v1/doctor", s.handle(s.doctor))
}

func (s *Server) doctor(r *http.Request) (int, any, error) {
	ctx := r.Context()
	now := s.now()
	workers, err := s.store.Workers(ctx, now)
	if err != nil {
		return 0, nil, err
	}
	repos, err := s.store.Repositories(ctx)
	if err != nil {
		return 0, nil, err
	}
	kbAt, err := s.store.KbLastIndexedAt(ctx)
	if err != nil {
		return 0, nil, err
	}
	retained, err := s.store.RetainedWorktreeCount(ctx)
	if err != nil {
		return 0, nil, err
	}
	fiveHour, err := s.store.LatestSample(ctx, "five_hour")
	if err != nil {
		return 0, nil, err
	}
	sevenDay, err := s.store.LatestSample(ctx, "seven_day")
	if err != nil {
		return 0, nil, err
	}
	var startedAt time.Time
	if s.home != "" {
		if st, err := ReadState(s.home); err == nil && st != nil {
			startedAt = st.StartedAt
		}
	}
	checks := doctor.Daemon(doctor.DaemonInput{
		Version: s.version, SchemaVersion: s.store.SchemaVersion(), StartedAt: startedAt, Now: now,
		Workers: workers, Repositories: repos, KbLastIndexedAt: kbAt, RetainedCount: retained,
		FiveHourSample: fiveHour, SevenDaySample: sevenDay,
	})
	return http.StatusOK, checks, nil
}
