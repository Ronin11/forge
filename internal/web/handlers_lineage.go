package web

import (
	"net/http"

	"forge/internal/core/model"
	"forge/internal/core/store"
)

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
	return http.StatusOK, lineageResponse{RootID: ld.RootID, Nodes: nodes, DependencyEdges: edges}, nil
}
