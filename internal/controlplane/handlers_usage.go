package controlplane

import (
	"context"
	"net/http"

	"forge/internal/core/protocol"
	"forge/internal/store"
)

// usageReporter is the capability a SchedulerPolicy exposes when it can report
// budget usage (STYLE.md §1: a needed capability is an interface the caller
// checks, never a type switch on implementations). *BudgetPolicy implements it;
// AdmitAll does not, and the usage endpoint answers 501 until the budget policy
// is active.
type usageReporter interface {
	Usage(ctx context.Context) (Usage, error)
	Config() BudgetConfig
}

// usageResponse is GET /api/v1/usage's wire shape.
type usageResponse struct {
	SchemaVersion int         `json:"schema_version"`
	Usage         Usage       `json:"usage"`
	Config        usageConfig `json:"config"`
}

// usageConfig echoes the [budget] thresholds so clients can render usage
// against target without reading the daemon's config file.
type usageConfig struct {
	FiveHourTarget   float64 `json:"five_hour_target"`
	SevenDayTarget   float64 `json:"seven_day_target"`
	FiveHourHardStop float64 `json:"five_hour_hard_stop"`
	SevenDayHardStop float64 `json:"seven_day_hard_stop"`
	DailyUSDCap      float64 `json:"daily_usd_cap"`
}

// usageRoutes serves the budget usage report (DESIGN.md §10.1).
func (s *Server) usageRoutes(m *http.ServeMux) {
	m.HandleFunc("GET /api/v1/usage", s.handle(s.usage))
}

func (s *Server) usage(r *http.Request) (int, any, error) {
	rp, ok := s.policy.(usageReporter)
	if !ok {
		return http.StatusNotImplemented, protocol.Error{Error: "budget policy not active"}, nil
	}
	u, err := rp.Usage(r.Context())
	if err != nil {
		return 0, nil, err
	}
	cfg := rp.Config()
	return http.StatusOK, usageResponse{
		SchemaVersion: 1,
		Usage:         u,
		Config: usageConfig{
			FiveHourTarget: cfg.FiveHourTarget, SevenDayTarget: cfg.SevenDayTarget,
			FiveHourHardStop: cfg.FiveHourHardStop, SevenDayHardStop: cfg.SevenDayHardStop,
			DailyUSDCap: cfg.DailyUSDCap,
		},
	}, nil
}

// journalBudgetResets writes one "budget.reset" journal row per resets_at
// change the incoming samples reveal, in the same transaction that inserts
// them — the durable unspent_at_reset metric of DESIGN.md §10.2. The previous
// sample is read from the reader pool, which is complete for samples because
// they are only written in earlier, committed transactions. Without an active
// budget policy there is no target to measure unspent against, so nothing is
// journaled.
func (s *Server) journalBudgetResets(ctx context.Context, tx *store.Tx, samples []store.RateLimitSample) error {
	if len(samples) == 0 {
		return nil
	}
	rp, ok := s.policy.(usageReporter)
	if !ok {
		return nil
	}
	prev := map[string]*store.RateLimitSample{}
	for _, w := range []string{"five_hour", "seven_day"} {
		p, err := s.store.LatestSample(ctx, w)
		if err != nil {
			return err
		}
		prev[w] = p
	}
	for _, b := range budgetResets(rp.Config(), prev, samples) {
		if err := tx.Journal(ctx, "budget.reset", store.EntityDaemon, "budget", b); err != nil {
			return err
		}
	}
	return nil
}
