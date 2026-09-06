package web

import (
	"context"
	"net/http"
	"os/exec"
	"strconv"
	"strings"
	"time"

	"forge/internal/core/daemon"
	"forge/internal/core/doctor"
	"forge/internal/core/plugin"
	"forge/internal/core/store"
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
	plugins, err := s.store.Plugins(ctx)
	if err != nil {
		return 0, nil, err
	}
	var pluginHealth []plugin.PluginHealth
	if s.pluginHealth != nil {
		pluginHealth = s.pluginHealth()
	}
	var startedAt time.Time
	if s.home != "" {
		if st, err := daemon.ReadState(s.home); err == nil && st != nil {
			startedAt = st.StartedAt
		}
	}
	pricing, err := s.pricingPairs(ctx, now)
	if err != nil {
		return 0, nil, err
	}
	checks := doctor.Daemon(doctor.DaemonInput{
		Version: s.version, SchemaVersion: s.store.SchemaVersion(), StartedAt: startedAt, Now: now,
		Workers: workers, Repositories: repos, KbLastIndexedAt: kbAt, RetainedCount: retained,
		FiveHourSample: fiveHour, SevenDaySample: sevenDay,
		AheadOfOrigin: s.reposAheadOfOrigin(ctx, repos),
		Plugins:       plugins, PluginHealth: pluginHealth, PricingPairs: pricing,
		PluginDenials: s.pluginDenials(ctx, now),
	})
	return http.StatusOK, checks, nil
}

// pricingDriftWindow bounds how far back the doctor's pricing-drift check looks.
const pricingDriftWindow = 7 * 24 * time.Hour

// pricingPairs gathers recent attempts that carry both a notional cost (usd,
// tokens × Forge's price table) and the executor's self-reported cost, for the
// doctor's price-table staleness check (DESIGN.md §21).
func (s *Server) pricingPairs(ctx context.Context, now time.Time) ([]doctor.PricingPair, error) {
	facts, err := s.store.FactsSince(ctx, now.Add(-pricingDriftWindow), now.Add(time.Hour), "")
	if err != nil {
		return nil, err
	}
	var pairs []doctor.PricingPair
	for _, f := range facts {
		if f.USD == nil || f.CostUSD == nil {
			continue
		}
		pairs = append(pairs, doctor.PricingPair{Notional: *f.USD, Reported: *f.CostUSD})
	}
	return pairs, nil
}

// pluginDenials tallies recent scope-table denials for the doctor; best
// effort — an error just drops the check.
func (s *Server) pluginDenials(ctx context.Context, now time.Time) []doctor.PluginDenial {
	rows, err := s.store.PluginDenialsSince(ctx, now.Add(-48*time.Hour))
	if err != nil {
		s.log.WarnContext(ctx, "doctor: plugin denials", "error", err)
		return nil
	}
	var out []doctor.PluginDenial
	for _, r := range rows {
		out = append(out, doctor.PluginDenial{Plugin: r.Plugin, Count: r.Count, LastPath: r.LastPath})
	}
	return out
}

// reposAheadOfOrigin measures, per live non-bench repository, how far the
// local checked-out branch outruns its upstream — the stale-base check's
// input. A repo without an upstream (or any git failure) contributes nothing.
func (s *Server) reposAheadOfOrigin(ctx context.Context, repos []store.Repository) []doctor.RepoAhead {
	var out []doctor.RepoAhead
	for _, r := range repos {
		if r.Archived || strings.HasPrefix(r.Name, "bench-") || r.Path == "" {
			continue
		}
		cctx, cancel := context.WithTimeout(ctx, 5*time.Second)
		branchB, err := exec.CommandContext(cctx, "git", "-C", r.Path, "rev-parse", "--abbrev-ref", "HEAD").Output()
		if err != nil {
			cancel()
			continue
		}
		countB, err := exec.CommandContext(cctx, "git", "-C", r.Path, "rev-list", "--count", "@{u}..HEAD").Output()
		cancel()
		if err != nil {
			continue // no upstream configured: nothing to compare
		}
		n, _ := strconv.Atoi(strings.TrimSpace(string(countB)))
		out = append(out, doctor.RepoAhead{Name: r.Name, Branch: strings.TrimSpace(string(branchB)), Ahead: n})
	}
	return out
}
