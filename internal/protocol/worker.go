package protocol

import (
	"encoding/json"
	"time"

	"forge/internal/model"
)

// Repository is what a worker advertises for each configured checkout.
type Repository struct {
	Name           string `json:"name"`
	Path           string `json:"path"`
	OriginIdentity string `json:"origin_identity"`
	BaseBranch     string `json:"base_branch,omitempty"`
	Project        string `json:"project"`
}

// RetainedWorktree is one worktree the worker kept, with the command to remove it.
type RetainedWorktree struct {
	AttemptID      string `json:"attempt_id"`
	Path           string `json:"path"`
	Reason         string `json:"reason"`
	CleanupCommand string `json:"cleanup_command"`
}

// RegisterRequest is sent every 30 s and doubles as liveness.
type RegisterRequest struct {
	WorkerID      string             `json:"worker_id"`
	Name          string             `json:"name"`
	Version       string             `json:"version"`
	MaxConcurrent int                `json:"max_concurrent"`
	Active        int                `json:"active"`
	Executors     []string           `json:"executors"`
	Capabilities  map[string]string  `json:"capabilities"` // browser, sandbox, runner:<name> → ready|missing|…
	Repositories  []Repository       `json:"repositories"`
	Retained      []RetainedWorktree `json:"retained"`
}

// RegisterResponse carries what the daemon wants the worker to know each tick.
type RegisterResponse struct {
	LogLevels string `json:"log_levels"` // logging.Levels grammar; applied by the worker
}

// ClaimRequest asks for one Target. ClaimRequestID dedups retries after a lost
// response; LeaseToken is minted by the worker and only its hash is stored.
type ClaimRequest struct {
	WorkerID       string `json:"worker_id"`
	ClaimRequestID string `json:"claim_request_id"`
	LeaseToken     string `json:"lease_token"`
}

// Claim is the frozen snapshot a worker executes. It never contains mutable
// routine state.
type Claim struct {
	AttemptID      string            `json:"attempt_id"`
	TargetID       string            `json:"target_id"`
	WorkID         string            `json:"work_id"`
	RoutineName    string            `json:"routine_name"`
	Generation     int               `json:"generation"`
	Repository     string            `json:"repository"`
	Mode           string            `json:"mode"`
	Prompt         string            `json:"prompt"`
	Executor       string            `json:"executor"`
	Model          string            `json:"model"`
	Effort         string            `json:"effort,omitempty"`
	MaxTurns       int               `json:"max_turns"`
	TimeoutSeconds int               `json:"timeout_seconds"`
	MaxBudgetUSD   float64           `json:"max_budget_usd,omitempty"`
	AllowedTools   []string          `json:"allowed_tools,omitempty"`
	Autonomy       model.Autonomy    `json:"autonomy"`
	BudgetClass    model.BudgetClass `json:"budget_class"`
	Trigger        model.Trigger     `json:"trigger"`
	LeaseExpiresAt time.Time         `json:"lease_expires_at"`
	Integrate      bool              `json:"integrate"`
	// MCPToken is minted per attempt; forge mcp presents it and is narrowed to this
	// attempt's routes. Only its hash is stored.
	MCPToken string `json:"mcp_token"`
	// Policy is what the worker needs from the daemon's config so no second config
	// file exists on the worker side.
	Policy Policy `json:"policy"`
	// Resume is set when the Target is being resumed after a human answer.
	Resume *Resume `json:"resume,omitempty"`
	// Snapshot is the whole frozen routine for anything the fields above omit.
	Snapshot json.RawMessage `json:"snapshot,omitempty"`
}

// Policy is daemon config the worker applies to one attempt.
type Policy struct {
	RequireSandbox bool              `json:"require_sandbox"`
	AllowHosts     []string          `json:"allow_hosts,omitempty"`
	GitConfig      map[string]string `json:"git_config,omitempty"` // passed as GIT_CONFIG_* env
}

// Resume tells the worker to continue an existing attempt's session.
type Resume struct {
	SessionID string `json:"session_id"`
	Answer    string `json:"answer"`
	Launches  int    `json:"launches"`
}

// HeartbeatRequest renews the lease and reports the phase.
type HeartbeatRequest struct {
	LeaseToken    string          `json:"lease_token"`
	Phase         string          `json:"phase"`
	State         model.State     `json:"state,omitempty"` // preparing | running, when it changes
	PID           int             `json:"pid,omitempty"`
	PIDStart      int64           `json:"pid_start,omitempty"`
	SessionID     string          `json:"session_id,omitempty"`
	PromptVersion *PromptVersion  `json:"prompt_version,omitempty"`
	ForgeToml     json.RawMessage `json:"forge_toml,omitempty"` // the repo's declared checks etc.
	Worktree      string          `json:"worktree,omitempty"`
	Branch        string          `json:"branch,omitempty"`
	BaseBranch    string          `json:"base_branch,omitempty"`
	BaseCommit    string          `json:"base_commit,omitempty"`
}

// HeartbeatResponse returns the cancel flag and the new expiry.
type HeartbeatResponse struct {
	CancelRequested bool      `json:"cancel_requested"`
	LeaseExpiresAt  time.Time `json:"lease_expires_at"`
	LogLevels       string    `json:"log_levels,omitempty"`
}

// PromptVersion links an attempt to the exact prompt configuration it ran with.
type PromptVersion struct {
	Hash            string   `json:"hash"`
	Routine         string   `json:"routine"`
	Generation      int      `json:"generation"`
	Mode            string   `json:"mode"`
	Template        string   `json:"template"`
	RenderedExample string   `json:"rendered_example"`
	SystemAppend    string   `json:"system_append"`
	ToolList        []string `json:"tool_list"`
	Model           string   `json:"model"`
	Effort          string   `json:"effort"`
}

// Usage is the executor's authoritative token totals.
type Usage struct {
	InputTokens         int64 `json:"input_tokens"`
	OutputTokens        int64 `json:"output_tokens"`
	CacheReadTokens     int64 `json:"cache_read_tokens"`
	CacheCreationTokens int64 `json:"cache_creation_tokens"`
}

// GitOutcome is what the worker measured after the agent exited.
type GitOutcome struct {
	Dirty        bool     `json:"dirty"`
	Commits      int      `json:"commits"`
	FilesChanged int      `json:"files_changed"`
	Insertions   int      `json:"insertions"`
	Deletions    int      `json:"deletions"`
	Pushed       bool     `json:"pushed"`
	Head         string   `json:"head"`
	ChangedPaths []string `json:"changed_paths,omitempty"`
}

// Cleanup is the decision the worker took and why.
type Cleanup struct {
	Outcome string `json:"outcome"` // removed | retained | missing
	Reason  string `json:"reason"`
	Command string `json:"command,omitempty"`
}

// Verification is the worker-side (L0/L1) result.
type Verification struct {
	Level   int             `json:"level"`  // highest level attempted
	Passed  bool            `json:"passed"` // whether the required level passed so far
	Reason  string          `json:"reason,omitempty"`
	Verdict json.RawMessage `json:"verdict,omitempty"`
}

// CompleteRequest is the terminal report. State is what the worker observed:
// succeeded (exit 0 with a parsed result), waiting_human (open Question), failed,
// or cancelled; the daemon runs the transition and decides verification.
type CompleteRequest struct {
	LeaseToken      string              `json:"lease_token"`
	State           model.State         `json:"state"`
	FailureReason   model.FailureReason `json:"failure_reason,omitempty"`
	ExitCode        int                 `json:"exit_code"`
	IsError         bool                `json:"is_error"`
	ResultText      string              `json:"result_text,omitempty"`
	Result          json.RawMessage     `json:"result,omitempty"`
	NumTurns        int                 `json:"num_turns"`
	Usage           Usage               `json:"usage"`
	CostUSD         *float64            `json:"cost_usd,omitempty"`
	SessionID       string              `json:"session_id,omitempty"`
	Launches        int                 `json:"launches"`
	Git             GitOutcome          `json:"git"`
	Verification    Verification        `json:"verification"`
	Cleanup         Cleanup             `json:"cleanup"`
	OutputPath      string              `json:"output_path"`
	OutputBytes     int64               `json:"output_bytes"`
	OutputTruncated bool                `json:"output_truncated"`
	Question        *QuestionRequest    `json:"question,omitempty"`
	StartedAt       time.Time           `json:"started_at"`
	FinishedAt      time.Time           `json:"finished_at"`
}

// QuestionRequest is a needs_input result or a forge_ask call.
type QuestionRequest struct {
	Text       string          `json:"text"`
	Options    []string        `json:"options,omitempty"`
	Context    json.RawMessage `json:"context,omitempty"`
	Checkpoint string          `json:"checkpoint,omitempty"`
}

// CompleteResponse tells the worker the daemon's view, including whether this was
// a late completion for a Target the sweeper already closed.
type CompleteResponse struct {
	State model.State `json:"state"`
	Late  bool        `json:"late"`
}

// CleanupPatch updates only cleanup and git fields (reconcile after a restart).
type CleanupPatch struct {
	Git     *GitOutcome `json:"git,omitempty"`
	Cleanup Cleanup     `json:"cleanup"`
}

// Handshake is GET /api/v1/handshake.
type Handshake struct {
	Version       string `json:"version"`
	SchemaVersion string `json:"schema_version"`
	State         string `json:"state"` // running | draining
	PID           int    `json:"pid"`
}

// Error is every error body.
type Error struct {
	Error string `json:"error"`
}
