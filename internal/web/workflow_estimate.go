package web

// The workflow cost estimate: what a run of this graph is expected to spend,
// priced per routine node from the same 30-day attempt-facts history the
// router uses (p50 notional USD, routine+model narrowed, model-wide as
// fallback). A node with no history is reported as unknown rather than
// priced from a made-up number — the estimate names how much of the graph it
// actually covers. Script/switch/join nodes cost nothing.

import (
	"net/http"
	"time"

	"forge/internal/core/store"
)

// workflowNodeEstimate is one routine node's expected spend.
type workflowNodeEstimate struct {
	Node    string `json:"node"`
	Routine string `json:"routine"`
	Model   string `json:"model,omitempty"`
	// USD is the p50 notional cost of one attempt; nil = no history.
	USD     *float64 `json:"usd,omitempty"`
	Samples int      `json:"samples"`
	// Source says how the figure was reached: "routine" (this routine on
	// this model), "model" (the model across all routines), "none".
	Source string `json:"source"`
	// LoopCap is the max_iterations of a loop edge re-entering this node;
	// its cost can repeat up to that many times.
	LoopCap int `json:"loop_cap,omitempty"`
}

// workflowEstimate is GET /api/v1/workflows/{name}/estimate.
type workflowEstimate struct {
	Workflow   string                 `json:"workflow"`
	Generation int                    `json:"generation"`
	Nodes      []workflowNodeEstimate `json:"nodes"`
	// KnownUSD sums the priced nodes (once each — loop and branch caveats
	// are the reader's); KnownNodes of RoutineNodes carry a figure.
	KnownUSD     float64 `json:"known_usd"`
	KnownNodes   int     `json:"known_nodes"`
	RoutineNodes int     `json:"routine_nodes"`
	// Conditional is set when branching (switch, failure, or case edges)
	// means not every node necessarily runs.
	Conditional bool `json:"conditional"`
}

func (s *Server) estimateWorkflow(r *http.Request) (int, any, error) {
	ctx := r.Context()
	w, err := s.store.GetWorkflow(ctx, r.PathValue("name"))
	if err != nil {
		return 0, nil, err
	}
	out := workflowEstimate{Workflow: w.Name, Generation: w.Generation, Nodes: []workflowNodeEstimate{}}
	if w.Graph == nil {
		return http.StatusOK, out, nil
	}
	graph := w.Graph

	loopCap := map[string]int{}
	for _, e := range graph.Edges {
		if e.Loop && e.MaxIterations > loopCap[e.To] {
			loopCap[e.To] = e.MaxIterations
		}
		if e.When == store.WhenFailure || e.When == store.WhenCase {
			out.Conditional = true
		}
	}
	for _, n := range graph.Nodes {
		if n.Type == store.NodeSwitch {
			out.Conditional = true
		}
	}

	since, until := s.now().Add(-routingWindow), s.now().Add(time.Hour)
	var globalByModel map[string][]store.AttemptFacts
	globalFacts := func() map[string][]store.AttemptFacts {
		if globalByModel == nil {
			rows, err := s.store.FactsSince(ctx, since, until, "")
			if err != nil {
				s.log.WarnContext(ctx, "workflow estimate: global facts", "error", err)
				rows = nil
			}
			globalByModel = groupFactsByModel(rows)
		}
		return globalByModel
	}
	routineByModel := map[string]map[string][]store.AttemptFacts{}

	for _, n := range graph.Nodes {
		var ne workflowNodeEstimate
		switch n.Type {
		case store.NodeRoutine:
			cfg, err := n.RoutineConfig()
			if err != nil {
				continue // Validate rejects these at save; an old row degrades to unknown
			}
			ne = workflowNodeEstimate{Node: n.ID, Routine: cfg.Routine, Source: "none", LoopCap: loopCap[n.ID]}
			rt, err := s.store.GetRoutine(ctx, cfg.Routine)
			if err == nil {
				ne.Model = rt.Model
				persona := rt.Persona
				if cfg.Persona != "" {
					persona = cfg.Persona
				}
				if ne.Model == "" && persona != "" {
					if lib := s.promptLibrary(); lib != nil {
						if p := lib.Persona(persona); p != nil {
							ne.Model = p.Model
						}
					}
				}
			}
		case store.NodeDirective:
			cfg, err := n.DirectiveConfig()
			if err != nil {
				continue
			}
			// History keys on the Work's routine name, which for a directive
			// node is the directive name itself.
			ne = workflowNodeEstimate{Node: n.ID, Routine: cfg.Directive, Source: "none", LoopCap: loopCap[n.ID]}
			ne.Model = cfg.Model
			if lib := s.promptLibrary(); lib != nil {
				if d := lib.Directive(cfg.Directive); d != nil {
					if ne.Model == "" {
						ne.Model = d.Model
					}
					persona := d.PersonaRef
					if cfg.Persona != "" {
						persona = cfg.Persona
					}
					if ne.Model == "" && persona != "" {
						if p := lib.Persona(persona); p != nil {
							ne.Model = p.Model
						}
					}
				}
			}
		default:
			continue
		}
		out.RoutineNodes++
		if ne.Model != "" {
			if routineByModel[ne.Routine] == nil {
				rows, err := s.store.FactsSince(ctx, since, until, ne.Routine)
				if err != nil {
					s.log.WarnContext(ctx, "workflow estimate: routine facts", "routine", ne.Routine, "error", err)
				}
				routineByModel[ne.Routine] = groupFactsByModel(rows)
			}
			rows, source := routineByModel[ne.Routine][ne.Model], "routine"
			if len(rows) == 0 {
				rows, source = globalFacts()[ne.Model], "model"
			}
			if usd := p50OfPtr(rows, func(f store.AttemptFacts) *float64 { return f.USD }); usd != nil {
				ne.USD, ne.Samples, ne.Source = usd, len(rows), source
				out.KnownUSD += *usd
				out.KnownNodes++
			}
		}
		out.Nodes = append(out.Nodes, ne)
	}
	return http.StatusOK, out, nil
}
