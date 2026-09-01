package ui

import (
	"sort"

	"forge/internal/core/model"
	"forge/internal/core/store"
	"forge/internal/web"
)

// --- UI view models over a web.LineageData (task detail strip + /work tree) ---

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

func lineageRef(ld *web.LineageData, id string, cause model.Cause) provRef {
	w := ld.ByID[id]
	return provRef{ID: id, Title: w.Title, State: ld.State[id], Cause: cause}
}

func lineageStrip(ld *web.LineageData, workID string) provStrip {
	self := ld.ByID[workID]
	s := provStrip{RootID: ld.RootID, MultiNode: len(ld.Works) > 1}
	// Breadcrumb: walk caused_by to the root, then reverse into root → parent.
	for id := self.CausedByWorkID; id != ""; {
		w, ok := ld.ByID[id]
		if !ok {
			break
		}
		s.Ancestors = append([]provRef{lineageRef(ld, id, w.Cause)}, s.Ancestors...)
		id = w.CausedByWorkID
	}
	if _, ok := ld.ByID[self.CausedByWorkID]; ok && self.CausedByWorkID != "" {
		// The parent edge is labelled by THIS Work's cause ("planned by …").
		p := lineageRef(ld, self.CausedByWorkID, self.Cause)
		s.Parent = &p
	}
	for _, w := range ld.Works {
		if w.CausedByWorkID == workID {
			s.Children = append(s.Children, lineageRef(ld, w.ID, w.Cause))
		}
	}
	for _, e := range ld.Edges {
		if e.Work == workID {
			s.BlockedBy = append(s.BlockedBy, lineageRef(ld, e.BlockedBy, ld.ByID[e.BlockedBy].Cause))
		}
		if e.BlockedBy == workID {
			s.Blocks = append(s.Blocks, lineageRef(ld, e.Work, ld.ByID[e.Work].Cause))
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
func lineageBuildTree(ld *web.LineageData, attemptsByTarget map[string][]store.Attempt) (*treeNode, workRollup) {
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
		w := ld.ByID[id]
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
