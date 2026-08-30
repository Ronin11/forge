package tools

// The repository tools run inside `forge mcp` itself — a child of the agent,
// in the worktree, in the agent's process group — so a group kill takes its
// checks with it and the control plane never spawns processes in a
// worker-owned directory (DESIGN.md §13). The daemon registers name,
// description, and schema only; forge mcp executes them.

import (
	"context"
	"encoding/json"
	"fmt"
)

// localTool describes one repository tool the daemon never executes.
type localTool struct {
	name        string
	description string
	schema      string
}

func (t localTool) Name() string                 { return t.name }
func (t localTool) Description() string          { return t.description }
func (t localTool) InputSchema() json.RawMessage { return json.RawMessage(t.schema) }
func (t localTool) Where() string                { return WhereLocal }

func (t localTool) Call(context.Context, Request) (json.RawMessage, error) {
	return nil, fmt.Errorf("%s is a local tool; forge mcp executes it", t.name)
}

// localTools is the repository set Defaults registers.
func localTools() []Tool {
	return []Tool{
		localTool{
			name:        "forge_repo_status",
			description: "Branch, base branch and commit, dirty state, and ahead/behind of the attempt's worktree.",
			schema:      emptySchema,
		},
		localTool{
			name:        "forge_check",
			description: "Run the repository's declared checks (or one by name) and report {check, passed, duration_us, failing_tests, output_tail} per check.",
			schema:      `{"type":"object","properties":{"check":{"type":"string","description":"one declared check; all when omitted"}},"additionalProperties":false}`,
		},
		localTool{
			name:        "forge_diff_summary",
			description: "Summary of the worktree's diff against the base commit: files changed, insertions, deletions, per-file stats.",
			schema:      emptySchema,
		},
	}
}
