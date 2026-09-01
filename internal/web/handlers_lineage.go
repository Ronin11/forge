package web

import (
	"context"
	"net/http"
	"sort"

	"forge/internal/core/engine"
	"forge/internal/core/model"
	"forge/internal/core/store"
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

// --- UI view models over a lineageData (task detail strip + /work tree) ---

// provRef is one Work referenced in the task-detail provenance strip.
type provRef struct {
	ID    string
	Title string
	State model.WorkState
	Cause model.Cause
}

// provStrip is the task-detail provenance strip (DESIGN.md §3): the breadcrumb
// from the root down to the parent (zoom out), the immediate cause and blocking
// deps (backward), and the children and blocked Works (forward). A lone Work (a
// root with no tree) has MultiNode false and renders nothing.
type provStrip struct {
	RootID    string
	MultiNode bool
	Ancestors []provRef // root … parent, in that order
	Parent    *provRef  // the caused_by parent, labelled by this Work's own cause
	BlockedBy []provRef
	Children  []provRef
	Blocks    []provRef
}

func (ld *lineageData) ref(id string, cause model.Cause) provRef {
	w := ld.byID[id]
	return provRef{ID: id, Title: w.Title, State: ld.State[id], Cause: cause}
}

func (ld *lineageData) strip(workID string) provStrip {
	self := ld.byID[workID]
	s := provStrip{RootID: ld.RootID, MultiNode: len(ld.Works) > 1}
	// Breadcrumb: walk caused_by to the root, then reverse into root → parent.
	for id := self.CausedByWorkID; id != ""; {
		w, ok := ld.byID[id]
		if !ok {
			break
		}
		s.Ancestors = append([]provRef{ld.ref(id, w.Cause)}, s.Ancestors...)
		id = w.CausedByWorkID
	}
	if _, ok := ld.byID[self.CausedByWorkID]; ok && self.CausedByWorkID != "" {
		// The parent edge is labelled by THIS Work's cause ("planned by …").
		p := ld.ref(self.CausedByWorkID, self.Cause)
		s.Parent = &p
	}
	for _, w := range ld.Works {
		if w.CausedByWorkID == workID {
			s.Children = append(s.Children, ld.ref(w.ID, w.Cause))
		}
	}
	for _, e := range ld.Edges {
		if e.Work == workID {
			s.BlockedBy = append(s.BlockedBy, ld.ref(e.BlockedBy, ld.byID[e.BlockedBy].Cause))
		}
		if e.BlockedBy == workID {
			s.Blocks = append(s.Blocks, ld.ref(e.Work, ld.byID[e.Work].Cause))
		}
	}
	return s
}

// workRow is one Work in the /work tree view.
type workRow struct {
	Work      store.Work
	State     model.WorkState
	Depth     int
	Repos     []string
	Cost      *float64
	Duration  string
	BlockedBy []string // ids of blockers within the tree, rendered as chips
	IsPlan    bool     // has plan_task children → a "plan batch" divider chip
}

// treeNode is the nested tree the /work view renders (children in created
// order), so a subtree is one collapsible <details>.
type treeNode struct {
	Row      workRow
	Children []*treeNode
}

// stateCount is one derived-state tally in the /work header rollup.
type stateCount struct {
	State model.WorkState
	N     int
}

// workRollup aggregates every attempt in the tree for the /work header.
type workRollup struct {
	Cost         float64
	TokensIn     int64
	TokensOut    int64
	Attempts     int
	FilesChanged int
	Insertions   int
	Deletions    int
	States       []stateCount
}

// buildTree assembles the nested tree and the header rollup. attemptsByTarget is
// keyed by Target id; each attempt is summed exactly once (every Work is visited
// once during the DFS from the root).
func (ld *lineageData) buildTree(attemptsByTarget map[string][]store.Attempt) (*treeNode, workRollup) {
	childrenOf := map[string][]string{}
	hasPlanChild := map[string]bool{}
	for _, w := range ld.Works {
		if w.CausedByWorkID != "" {
			childrenOf[w.CausedByWorkID] = append(childrenOf[w.CausedByWorkID], w.ID)
			if w.Cause == model.CausePlanTask {
				hasPlanChild[w.CausedByWorkID] = true
			}
		}
	}
	blockedBy := map[string][]string{}
	for _, e := range ld.Edges {
		blockedBy[e.Work] = append(blockedBy[e.Work], e.BlockedBy)
	}
	var roll workRollup
	stateN := map[model.WorkState]int{}
	rowFor := func(id string) workRow {
		w := ld.byID[id]
		var repos []string
		var cost *float64
		for _, tgt := range ld.Targets[id] {
			repos = append(repos, tgt.Repository)
			for _, a := range attemptsByTarget[tgt.ID] {
				roll.Attempts++
				roll.TokensIn += a.Usage.InputTokens
				roll.TokensOut += a.Usage.OutputTokens
				roll.FilesChanged += a.Git.FilesChanged
				roll.Insertions += a.Git.Insertions
				roll.Deletions += a.Git.Deletions
				if a.CostUSD != nil {
					roll.Cost += *a.CostUSD
					v := *a.CostUSD
					if cost != nil {
						v += *cost
					}
					cost = &v
				}
			}
		}
		sort.Strings(repos)
		dur := "-"
		if !w.FinishedAt.IsZero() {
			dur = humanDuration(w.FinishedAt.Sub(w.CreatedAt))
		}
		stateN[ld.State[id]]++
		return workRow{Work: w, State: ld.State[id], Depth: ld.Depth[id], Repos: repos, Cost: cost, Duration: dur, BlockedBy: blockedBy[id], IsPlan: hasPlanChild[id]}
	}
	var build func(id string) *treeNode
	build = func(id string) *treeNode {
		n := &treeNode{Row: rowFor(id)}
		for _, cid := range childrenOf[id] {
			n.Children = append(n.Children, build(cid))
		}
		return n
	}
	root := build(ld.RootID)
	for _, s := range []model.WorkState{model.WorkSucceeded, model.WorkMerged, model.WorkFailed, model.WorkUnverified, model.WorkRunning, model.WorkMerging, model.WorkWaitingHuman, model.WorkConflict, model.WorkBlocked, model.WorkDeferred, model.WorkPending, model.WorkPartial, model.WorkCancelled} {
		if n := stateN[s]; n > 0 {
			roll.States = append(roll.States, stateCount{State: s, N: n})
		}
	}
	return root, roll
}
