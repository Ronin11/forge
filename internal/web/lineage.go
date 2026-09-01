package web

import (
	"context"

	"forge/internal/core/engine"
	"forge/internal/core/model"
	"forge/internal/core/store"
)

// LineageData is one provenance tree (DESIGN.md §3), resolved from any member
// id: the root, every Work sharing its root_work_id in created order, each
// Work's derived state and depth (caused_by hops from the root), the Targets
// behind the states, and the dependency edges internal to the tree. Both the
// lineage API and the UI (task detail, the /work view) build on it.
type LineageData struct {
	RootID  string
	Works   []store.Work
	State   map[string]model.WorkState
	Depth   map[string]int
	Targets map[string][]store.Target
	Edges   []model.Edge
	ByID    map[string]store.Work
}

// ComputeLineage resolves any Work id to its whole tree. A row whose
// root_work_id is empty (pre-backfill, never for rows this Forge wrote) is
// treated as a lone root so the page still renders.
func ComputeLineage(ctx context.Context, st *store.Store, anyID string) (*LineageData, error) {
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
	ByID := make(map[string]store.Work, len(works))
	for i, x := range works {
		ids[i], ByID[x.ID] = x.ID, x
	}
	targets, err := st.TargetsForWorks(ctx, ids)
	if err != nil {
		return nil, err
	}
	state := make(map[string]model.WorkState, len(works))
	for _, x := range works {
		state[x.ID] = model.DeriveWorkState(model.WorkInputs{Targets: engine.TargetStates(targets[x.ID]), Integrate: x.Integrate})
	}
	// Depth is caused_by hops from the root, memoized. caused_by never cycles
	// (a parent exists before its child), so the walk terminates.
	depth := make(map[string]int, len(works))
	var walk func(id string) int
	walk = func(id string) int {
		if v, ok := depth[id]; ok {
			return v
		}
		x, ok := ByID[id]
		if !ok || x.CausedByWorkID == "" || x.CausedByWorkID == id {
			depth[id] = 0
			return 0
		}
		if _, ok := ByID[x.CausedByWorkID]; !ok {
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
	return &LineageData{RootID: root, Works: works, State: state, Depth: depth, Targets: targets, Edges: edges, ByID: ByID}, nil
}

// lineageResponse is GET /api/v1/work/{id}/lineage: the whole tree of the id's
