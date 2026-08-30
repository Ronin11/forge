package controlplane

// The tool routes forge mcp calls (DESIGN.md §13): GET /api/v1/tools lists the
// registered tools with the calling attempt's context; POST /api/v1/tools/{name}
// dispatches one daemon tool. Auth differs from the worker routes: besides the
// worker token, the attempt's own per-attempt MCP token is accepted.

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
	"time"

	"forge/internal/model"
	"forge/internal/protocol"
	"forge/internal/store"
	"forge/internal/tools"
)

// toolCallTimeout bounds one daemon tool call. Long-running work (checks,
// agents) runs locally in forge mcp, so store-backed tools finishing later
// than this are stuck, not slow.
const toolCallTimeout = 15 * time.Minute

// authorizeTools is the tool routes' rule (DESIGN.md §1.1): open on the unix
// socket; on TCP either the worker token or the calling attempt's MCP token.
// The middleware deliberately lets /api/v1/tools* through with a missing or
// wrong worker token (see Handler) because only the handler knows the attempt
// id the MCP token is scoped to.
func (s *Server) authorizeTools(r *http.Request, attemptID string) (bool, error) {
	if s.transport(r.Context()) == transportUnix {
		return true, nil
	}
	if s.tokenOK(r) {
		return true, nil
	}
	presented, ok := strings.CutPrefix(r.Header.Get("Authorization"), "Bearer ")
	if !ok || presented == "" {
		return false, nil
	}
	return s.store.CheckMCPToken(r.Context(), attemptID, presented)
}

// errToolUnauthorized is the 401 body both tool routes answer.
var errToolUnauthorized = protocol.Error{Error: "worker or attempt token required"}

// resolveToolAttempt loads the attempt and its target so every tool sees the
// same per-attempt context (Repository and WorkID live on the target).
func (s *Server) resolveToolAttempt(ctx context.Context, id string) (tools.Attempt, error) {
	a, err := s.store.GetAttempt(ctx, id)
	if err != nil {
		return tools.Attempt{}, err
	}
	t, err := s.store.GetTarget(ctx, a.TargetID)
	if err != nil {
		return tools.Attempt{}, err
	}
	return tools.Attempt{
		ID: a.ID, TargetID: a.TargetID, WorkID: t.WorkID, Repository: t.Repository,
		WorktreePath: a.WorktreePath, Branch: a.Branch, BaseBranch: a.BaseBranch, BaseCommit: a.BaseCommit,
		Mode: a.Mode, Autonomy: a.Autonomy, Launches: a.Launches,
	}, nil
}

// toolDeps is what every tool call gets from the server.
func (s *Server) toolDeps() tools.Deps {
	return tools.Deps{Store: s.store, Write: s.store.Write, KbDir: s.kbDir, Clock: s.now, Logger: s.log}
}

// toolInfo is one row of GET /api/v1/tools.
type toolInfo struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	InputSchema json.RawMessage `json:"input_schema"`
	Where       string          `json:"where"`
}

// toolAttemptBody is the attempt context GET /api/v1/tools returns.
type toolAttemptBody struct {
	ID           string         `json:"id"`
	TargetID     string         `json:"target_id"`
	WorkID       string         `json:"work_id"`
	WorktreePath string         `json:"worktree_path"`
	Branch       string         `json:"branch"`
	BaseBranch   string         `json:"base_branch"`
	BaseCommit   string         `json:"base_commit"`
	Launches     int            `json:"launches"`
	Mode         string         `json:"mode"`
	Autonomy     model.Autonomy `json:"autonomy"`
	Repository   string         `json:"repository"`
}

func (s *Server) listTools(r *http.Request) (int, any, error) {
	ctx := r.Context()
	id := r.URL.Query().Get("attempt_id")
	if err := model.ValidateID(id); err != nil {
		return 0, nil, badRequest("attempt_id: %v", err)
	}
	ok, err := s.authorizeTools(r, id)
	if err != nil {
		return 0, nil, err
	}
	if !ok {
		return http.StatusUnauthorized, errToolUnauthorized, nil
	}
	att, err := s.resolveToolAttempt(ctx, id)
	if err != nil {
		return 0, nil, err
	}
	all := s.tools.All()
	infos := make([]toolInfo, 0, len(all))
	for _, t := range all {
		infos = append(infos, toolInfo{Name: t.Name(), Description: t.Description(), InputSchema: t.InputSchema(), Where: t.Where()})
	}
	return http.StatusOK, map[string]any{
		"schema_version": 1,
		"attempt": toolAttemptBody{
			ID: att.ID, TargetID: att.TargetID, WorkID: att.WorkID, WorktreePath: att.WorktreePath,
			Branch: att.Branch, BaseBranch: att.BaseBranch, BaseCommit: att.BaseCommit,
			Launches: att.Launches, Mode: att.Mode, Autonomy: att.Autonomy, Repository: att.Repository,
		},
		"tools": infos,
	}, nil
}

// toolCallBody is POST /api/v1/tools/{name}.
type toolCallBody struct {
	SchemaVersion int             `json:"schema_version"`
	AttemptID     string          `json:"attempt_id"`
	Input         json.RawMessage `json:"input"`
}

func (s *Server) callTool(r *http.Request) (int, any, error) {
	ctx := r.Context()
	name := r.PathValue("name")
	var body toolCallBody
	if err := decodeJSON(r, &body); err != nil {
		return 0, nil, err
	}
	if body.SchemaVersion != 0 && body.SchemaVersion != 1 {
		return 0, nil, badRequest("schema_version %d: want 1", body.SchemaVersion)
	}
	if err := model.ValidateID(body.AttemptID); err != nil {
		return 0, nil, badRequest("attempt_id: %v", err)
	}
	ok, err := s.authorizeTools(r, body.AttemptID)
	if err != nil {
		return 0, nil, err
	}
	if !ok {
		return http.StatusUnauthorized, errToolUnauthorized, nil
	}
	tool, ok := s.tools.Get(name)
	if !ok {
		return 0, nil, fmt.Errorf("tool %s: %w", name, store.ErrNotFound)
	}
	if tool.Where() == tools.WhereLocal {
		return 0, nil, badRequest("%s is a local tool; forge mcp executes it", name)
	}
	att, err := s.resolveToolAttempt(ctx, body.AttemptID)
	if err != nil {
		return 0, nil, err
	}
	callCtx, cancel := context.WithTimeout(ctx, toolCallTimeout)
	defer cancel()
	start := time.Now()
	out, err := tool.Call(callCtx, tools.Request{AttemptID: att.ID, Attempt: att, Input: body.Input, Deps: s.toolDeps()})
	if tools.IsBadInput(err) {
		return 0, nil, badRequest("%s: %v", name, err)
	}
	if err != nil {
		return 0, nil, fmt.Errorf("tool %s: %w", name, err)
	}
	s.log.DebugContext(ctx, "tool called", "tool", name, "attempt_id", att.ID, "output_bytes", len(out), "duration_us", time.Since(start).Microseconds())
	return http.StatusOK, map[string]any{"schema_version": 1, "output": out}, nil
}
