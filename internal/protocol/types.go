// Package protocol holds the JSON types shared by the control plane, the
// worker, and the CLI. It must not import controlplane or worker.
package protocol

import (
	"time"

	"forge/internal/model"
)

// Bounds shared by both sides.
const (
	MaxEventsPerAttempt = 2000
	MaxEventBytes       = 1 << 20 // per attempt, sum of message bytes
	MaxEventMessage     = 4 << 10 // one event line
	MaxResultBytes      = 256 << 10
	MaxErrorBytes       = 16 << 10
	LeaseDuration       = 30 * time.Second
	HeartbeatInterval   = 10 * time.Second
	RegisterInterval    = 30 * time.Second
	WorkerOfflineAfter  = 90 * time.Second
)

// Routine is a saved procedure.
type Routine struct {
	ID             string    `json:"id"`
	Name           string    `json:"name"`
	Prompt         string    `json:"prompt"`
	Repositories   []string  `json:"repositories"`
	Executor       string    `json:"executor"`
	Model          string    `json:"model"`
	MaxTurns       int       `json:"max_turns"`
	TimeoutSeconds int       `json:"timeout_seconds"`
	AllowedTools   []string  `json:"allowed_tools,omitempty"`
	Schedule       string    `json:"schedule,omitempty"`
	Enabled        bool      `json:"enabled"`
	Concurrency    int       `json:"concurrency"`
	Generation     int       `json:"generation"`
	CreatedAt      time.Time `json:"created_at"`
	UpdatedAt      time.Time `json:"updated_at"`
}

// Settings is the frozen executor configuration copied into a Work.
type Settings struct {
	Executor       string   `json:"executor"`
	Model          string   `json:"model"`
	MaxTurns       int      `json:"max_turns"`
	TimeoutSeconds int      `json:"timeout_seconds"`
	AllowedTools   []string `json:"allowed_tools,omitempty"`
}

// Work is one invocation of a Routine.
type Work struct {
	ID          string      `json:"id"`
	RoutineID   string      `json:"routine_id"`
	RoutineName string      `json:"routine_name"`
	Generation  int         `json:"generation"`
	Source      string      `json:"source"` // manual | schedule
	Prompt      string      `json:"prompt"`
	Settings    Settings    `json:"settings"`
	State       model.State `json:"state"`
	CreatedAt   time.Time   `json:"created_at"`
	Targets     []Target    `json:"targets,omitempty"`
}

// Target is one repository within one Work.
type Target struct {
	ID              string      `json:"id"`
	WorkID          string      `json:"work_id"`
	Repository      string      `json:"repository"`
	State           model.State `json:"state"`
	Reason          string      `json:"reason,omitempty"`
	Retained        bool        `json:"retained"`
	WorkerID        string      `json:"worker_id,omitempty"`
	LeaseExpiresAt  *time.Time  `json:"lease_expires_at,omitempty"`
	CancelRequested bool        `json:"cancel_requested"`
	CreatedAt       time.Time   `json:"created_at"`
	UpdatedAt       time.Time   `json:"updated_at"`
	Attempt         *Attempt    `json:"attempt,omitempty"`
}

// Usage is token usage reported by the executor, when available.
type Usage struct {
	InputTokens         int64 `json:"input_tokens"`
	OutputTokens        int64 `json:"output_tokens"`
	CacheReadTokens     int64 `json:"cache_read_tokens"`
	CacheCreationTokens int64 `json:"cache_creation_tokens"`
}

// Total returns every token counted.
func (u Usage) Total() int64 {
	return u.InputTokens + u.OutputTokens + u.CacheReadTokens + u.CacheCreationTokens
}

// GitOutcome is what the worker saw in the worktree after the executor exited.
type GitOutcome struct {
	Dirty      bool   `json:"dirty"`
	NewCommits int    `json:"new_commits"`
	Pushed     bool   `json:"pushed"`
	HeadCommit string `json:"head_commit,omitempty"`
}

// Cleanup is the worktree disposition after an attempt.
type Cleanup struct {
	Outcome string `json:"outcome"` // removed | retained | missing | pending
	Reason  string `json:"reason,omitempty"`
	Command string `json:"command,omitempty"`
}

// Attempt is one execution of a Target on one worker.
type Attempt struct {
	ID           string      `json:"id"`
	TargetID     string      `json:"target_id"`
	WorkID       string      `json:"work_id"`
	WorkerID     string      `json:"worker_id"`
	WorktreePath string      `json:"worktree_path,omitempty"`
	Branch       string      `json:"branch,omitempty"`
	BaseCommit   string      `json:"base_commit,omitempty"`
	PID          int         `json:"pid,omitempty"`
	StartedAt    *time.Time  `json:"started_at,omitempty"`
	FinishedAt   *time.Time  `json:"finished_at,omitempty"`
	ExitCode     *int        `json:"exit_code,omitempty"`
	Result       string      `json:"result,omitempty"`
	Error        string      `json:"error,omitempty"`
	NumTurns     int         `json:"num_turns,omitempty"`
	Usage        *Usage      `json:"usage,omitempty"`
	CostUSD      *float64    `json:"cost_usd,omitempty"`
	Git          *GitOutcome `json:"git,omitempty"`
	Cleanup      Cleanup     `json:"cleanup"`
	EventCount   int         `json:"event_count"`
	CreatedAt    time.Time   `json:"created_at"`
}

// Event is one bounded log line for an attempt.
type Event struct {
	Seq     int       `json:"seq"`
	Time    time.Time `json:"time"`
	Kind    string    `json:"kind"` // lifecycle | stdout | stderr
	Message string    `json:"message"`
}

// Worker is a registered worker as the control plane sees it.
type Worker struct {
	ID            string             `json:"id"`
	Name          string             `json:"name"`
	Version       string             `json:"version"`
	MaxConcurrent int                `json:"max_concurrent"`
	Active        int                `json:"active"`
	Executors     []string           `json:"executors"`
	Repositories  []Repository       `json:"repositories"`
	Retained      []RetainedWorktree `json:"retained,omitempty"`
	RegisteredAt  time.Time          `json:"registered_at"`
	LastSeenAt    time.Time          `json:"last_seen_at"`
	Online        bool               `json:"online"`
}

// Repository is a checkout advertised by a worker.
type Repository struct {
	Name           string `json:"name"`
	Path           string `json:"path"`
	RemoteIdentity string `json:"remote_identity"`
	BaseBranch     string `json:"base_branch"`
	WorkerID       string `json:"worker_id,omitempty"`
}

// RetainedWorktree is a worktree the worker kept for inspection.
type RetainedWorktree struct {
	AttemptID  string `json:"attempt_id"`
	Repository string `json:"repository"`
	Path       string `json:"path"`
	Branch     string `json:"branch"`
	Reason     string `json:"reason"`
	Command    string `json:"command"`
}

// RegisterRequest is sent by the worker on start and every RegisterInterval.
type RegisterRequest struct {
	ID            string             `json:"id"`
	Name          string             `json:"name"`
	Version       string             `json:"version"`
	MaxConcurrent int                `json:"max_concurrent"`
	Active        int                `json:"active"`
	Executors     []string           `json:"executors"`
	Repositories  []Repository       `json:"repositories"`
	Retained      []RetainedWorktree `json:"retained"`
}

// ClaimRequest asks for one pending Target the worker can run.
type ClaimRequest struct {
	WorkerID string `json:"worker_id"`
}

// Claim is a Target handed to a worker with its frozen inputs.
type Claim struct {
	Attempt        Attempt   `json:"attempt"`
	Target         Target    `json:"target"`
	WorkID         string    `json:"work_id"`
	RoutineName    string    `json:"routine_name"`
	Repository     string    `json:"repository"`
	Prompt         string    `json:"prompt"` // {{repo}} already resolved
	Settings       Settings  `json:"settings"`
	LeaseExpiresAt time.Time `json:"lease_expires_at"`
}

// HeartbeatRequest renews the lease and optionally advances the attempt.
type HeartbeatRequest struct {
	WorkerID     string      `json:"worker_id"`
	State        model.State `json:"state,omitempty"` // preparing | running, or empty
	WorktreePath string      `json:"worktree_path,omitempty"`
	Branch       string      `json:"branch,omitempty"`
	BaseCommit   string      `json:"base_commit,omitempty"`
	PID          int         `json:"pid,omitempty"`
}

// HeartbeatResponse carries the renewed lease and the cancel flag.
type HeartbeatResponse struct {
	LeaseExpiresAt  time.Time `json:"lease_expires_at"`
	CancelRequested bool      `json:"cancel_requested"`
}

// EventsRequest appends a batch of events to an attempt.
type EventsRequest struct {
	WorkerID string  `json:"worker_id"`
	Events   []Event `json:"events"`
}

// CompleteRequest reports the terminal outcome of an attempt. It is accepted
// after the Target is already terminal (lease expired) to record cleanup.
type CompleteRequest struct {
	WorkerID string      `json:"worker_id"`
	State    model.State `json:"state"` // succeeded | failed | cancelled
	Reason   string      `json:"reason,omitempty"`
	ExitCode *int        `json:"exit_code,omitempty"`
	Result   string      `json:"result,omitempty"`
	Error    string      `json:"error,omitempty"`
	NumTurns int         `json:"num_turns,omitempty"`
	Usage    *Usage      `json:"usage,omitempty"`
	CostUSD  *float64    `json:"cost_usd,omitempty"`
	Git      *GitOutcome `json:"git,omitempty"`
	Cleanup  Cleanup     `json:"cleanup"`
}

// RunRequest starts Work for a Routine; Repositories optionally narrows the set.
type RunRequest struct {
	Repositories []string `json:"repositories,omitempty"`
}

// Overview backs the dashboard.
type Overview struct {
	Running   []Work   `json:"running"`
	Recent    []Work   `json:"recent"`
	Attention []Target `json:"attention"`
	Workers   []Worker `json:"workers"`
}

// ErrorResponse is the JSON body of every non-2xx API response.
type ErrorResponse struct {
	Error string `json:"error"`
	Code  string `json:"code,omitempty"`
}
