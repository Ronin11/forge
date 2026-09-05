package web

import (
	"context"
	"encoding/json"
	"net/http"
	"time"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

type lineageResponse struct {
	RootID          string        `json:"root_id"`
	Nodes           []lineageNode `json:"nodes"`
	DependencyEdges []lineageEdge `json:"dependency_edges"`
	// Rollup is the ask's single answer-bearing view: what the whole tree
	// cost and where it landed.
	Rollup lineageRollup `json:"rollup"`
}

// lineageRollup aggregates a work tree: totals over every attempt, the
// outcome census, wall time from the root's creation to the last finish, and
// the newest supervise scores when a continuation rated the outcome.
type lineageRollup struct {
	Works         int             `json:"works"`
	Attempts      int             `json:"attempts"`
	CostUSD       float64         `json:"cost_usd"`
	TokensIn      int64           `json:"tokens_in"`
	TokensOut     int64           `json:"tokens_out"`
	WallSeconds   float64         `json:"wall_seconds,omitempty"`
	OutcomeCounts map[string]int  `json:"outcome_counts"`
	Size          string          `json:"size,omitempty"`
	Scores        json.RawMessage `json:"scores,omitempty"`
	Weakness      string          `json:"weakness,omitempty"`
	Open          int             `json:"open"`
}

// computeLineageRollup walks the tree's attempts. The scores come from the
// newest terminal supervise attempt's assessment — the most recent judgment
// of the whole ask.
func computeLineageRollup(ctx context.Context, st *store.Store, ld *LineageData) (lineageRollup, error) {
	out := lineageRollup{Works: len(ld.Works), OutcomeCounts: map[string]int{}}
	var targetIDs []string
	var lastFinish, lastSupervise time.Time
	for _, w := range ld.Works {
		out.OutcomeCounts[string(ld.State[w.ID])]++
		if w.FinishedAt.IsZero() {
			out.Open++
		} else if w.FinishedAt.After(lastFinish) {
			lastFinish = w.FinishedAt
		}
		if w.ID == ld.RootID {
			out.Size = w.Size
		}
		for _, t := range ld.Targets[w.ID] {
			targetIDs = append(targetIDs, t.ID)
		}
	}
	atts, err := st.AttemptsForTargets(ctx, targetIDs)
	if err != nil {
		return out, err
	}
	for _, list := range atts {
		for i := range list {
			a := &list[i]
			out.Attempts++
			if a.CostUSD != nil {
				out.CostUSD += *a.CostUSD
			}
			out.TokensIn += a.Usage.InputTokens
			out.TokensOut += a.Usage.OutputTokens
			if a.Mode == "supervise" && !a.FinishedAt.IsZero() && a.FinishedAt.After(lastSupervise) && len(a.Result) > 0 {
				var top struct {
					Assessment struct {
						Scores   json.RawMessage `json:"scores"`
						Weakness string          `json:"weakness"`
					} `json:"assessment"`
				}
				if json.Unmarshal(a.Result, &top) == nil && top.Assessment.Scores != nil {
					lastSupervise = a.FinishedAt
					out.Scores, out.Weakness = top.Assessment.Scores, top.Assessment.Weakness
				}
			}
		}
	}
	if root, ok := ld.ByID[ld.RootID]; ok && out.Open == 0 && !lastFinish.IsZero() && lastFinish.After(root.CreatedAt) {
		out.WallSeconds = lastFinish.Sub(root.CreatedAt).Seconds()
	}
	return out, nil
}

type lineageNode struct {
	Work  store.Work      `json:"work"`
	State model.WorkState `json:"state"`
	Depth int             `json:"depth"`
}

type lineageEdge struct {
	Work      string `json:"work"`
	BlockedBy string `json:"blocked_by"`
	On        string `json:"on"`
}

func (s *Server) workLineage(r *http.Request) (int, any, error) {
	id, err := pathID(r)
	if err != nil {
		return 0, nil, err
	}
	ld, err := ComputeLineage(r.Context(), s.store, id)
	if err != nil {
		return 0, nil, err
	}
	nodes := make([]lineageNode, len(ld.Works))
	for i, w := range ld.Works {
		nodes[i] = lineageNode{Work: w, State: ld.State[w.ID], Depth: ld.Depth[w.ID]}
	}
	edges := make([]lineageEdge, 0, len(ld.Edges))
	for _, e := range ld.Edges {
		edges = append(edges, lineageEdge{Work: e.Work, BlockedBy: e.BlockedBy, On: string(e.On)})
	}
	rollup, err := computeLineageRollup(r.Context(), s.store, ld)
	if err != nil {
		return 0, nil, err
	}
	return http.StatusOK, lineageResponse{RootID: ld.RootID, Nodes: nodes, DependencyEdges: edges, Rollup: rollup}, nil
}
