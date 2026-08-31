package controlplane

import (
	"context"
	"net/http"

	"forge/internal/model"
	"forge/internal/store"
)

// lineageData is one provenance tree (DESIGN.md §3), resolved from any member
// id: the root, every Work sharing its root_work_id in created order, each
// Work's derived state and depth (caused_by hops from the root), the Targets
// behind the states, and the dependency edges internal to the tree. Both the
// lineage API and the UI (task detail, the /work view) build on it.
type lineageData struct {
	RootID  string
	Works   []store.Work
	State   map[string]model.WorkState
	Depth   map[string]int
	Targets map[string][]store.Target
	Edges   []model.Edge
	byID    map[string]store.Work
}

// computeLineage resolves any Work id to its whole tree. A row whose
// root_work_id is empty (pre-backfill, never for rows this Forge wrote) is
// treated as a lone root so the page still renders.
func computeLineage(ctx context.Context, st *store.Store, anyID string) (*lineageData, error) {
	w, err := st.GetWork(ctx, anyID)
	if err != nil {
		return nil, err
	}
	root := w.RootWorkID
	if root == "" {
		root = w.ID
	}
	works, err := st.WorkTree(ctx, root)
	if err != nil {
		return nil, err
	}
	if len(works) == 0 {
		works, root = []store.Work{*w}, w.ID
	}
	ids := make([]string, len(works))
	byID := make(map[string]store.Work, len(works))
	for i, x := range works {
		ids[i], byID[x.ID] = x.ID, x
	}
	targets, err := st.TargetsForWorks(ctx, ids)
	if err != nil {
		return nil, err
	}
	state := make(map[string]model.WorkState, len(works))
	for _, x := range works {
		state[x.ID] = model.DeriveWorkState(model.WorkInputs{Targets: targetStates(targets[x.ID]), Integrate: x.Integrate})
	}
	// Depth is caused_by hops from the root, memoized. caused_by never cycles
	// (a parent exists before its child), so the walk terminates.
	depth := make(map[string]int, len(works))
	var walk func(id string) int
	walk = func(id string) int {
		if v, ok := depth[id]; ok {
			return v
		}
		x, ok := byID[id]
		if !ok || x.CausedByWorkID == "" || x.CausedByWorkID == id {
			depth[id] = 0
			return 0
		}
		if _, ok := byID[x.CausedByWorkID]; !ok {
			depth[id] = 0
			return 0
		}
		depth[id] = walk(x.CausedByWorkID) + 1
		return depth[id]
	}
	for _, x := range works {
		walk(x.ID)
	}
	edges, err := st.WorkDependenciesWithin(ctx, ids)
	if err != nil {
		return nil, err
	}
	return &lineageData{RootID: root, Works: works, State: state, Depth: depth, Targets: targets, Edges: edges, byID: byID}, nil
}

// lineageResponse is GET /api/v1/work/{id}/lineage: the whole tree of the id's
// root, each node's derived state and depth, and the dependency edges within.
type lineageResponse struct {
	RootID          string        `json:"root_id"`
	Nodes           []lineageNode `json:"nodes"`
	DependencyEdges []lineageEdge `json:"dependency_edges"`
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
	ld, err := computeLineage(r.Context(), s.store, id)
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
	return http.StatusOK, lineageResponse{RootID: ld.RootID, Nodes: nodes, DependencyEdges: edges}, nil
}
