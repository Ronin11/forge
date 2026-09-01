package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	"forge/internal/model"
	"forge/internal/protocol"
)

// Attempt is one execution of a Target.
type Attempt struct {
	ID             string `json:"id"`
	TargetID       string `json:"target_id"`
	WorkerID       string `json:"worker_id"`
	ClaimRequestID string `json:"claim_request_id"`
	Executor       string `json:"executor"`
	Model          string `json:"model"`
	ModelAlias     string `json:"model_alias"`
	// Runner is the M10 runner the chosen model runs on (DESIGN.md §21);
	// EscalatedFrom is the previous attempt's model alias when this attempt is
	// the next rung of an escalation ladder; Routing is the JSON routing
	// decision the task-detail UI reads. All empty on a plain M1-style claim.
	Runner            string              `json:"runner,omitempty"`
	EscalatedFrom     string              `json:"escalated_from,omitempty"`
	Routing           json.RawMessage     `json:"routing,omitempty"`
	Effort            string              `json:"effort,omitempty"`
	Mode              string              `json:"mode"`
	Autonomy          model.Autonomy      `json:"autonomy"`
	WorktreePath      string              `json:"worktree_path,omitempty"`
	Branch            string              `json:"branch,omitempty"`
	BaseBranch        string              `json:"base_branch,omitempty"`
	BaseCommit        string              `json:"base_commit,omitempty"`
	StackBaseCommit   string              `json:"stack_base_commit,omitempty"`
	HeadCommit        string              `json:"head_commit,omitempty"`
	PID               int                 `json:"pid,omitempty"`
	PIDStart          int64               `json:"pid_start,omitempty"`
	SessionID         string              `json:"session_id,omitempty"`
	PromptVersionHash string              `json:"prompt_version_hash,omitempty"`
	Launches          int                 `json:"launches"`
	StartedAt         time.Time           `json:"started_at,omitempty"`
	FinishedAt        time.Time           `json:"finished_at,omitempty"`
	ExitCode          *int                `json:"exit_code,omitempty"`
	FailureReason     model.FailureReason `json:"failure_reason,omitempty"`
	UnverifiedReason  string              `json:"unverified_reason,omitempty"`
	IsError           bool                `json:"is_error"`
	ResultText        string              `json:"result_text,omitempty"`
	Result            json.RawMessage     `json:"result,omitempty"`
	NumTurns          int                 `json:"num_turns"`
	Usage             protocol.Usage      `json:"usage"`
	CostUSD           *float64            `json:"cost_usd,omitempty"`
	Git               protocol.GitOutcome `json:"git"`
	VerificationLevel *int                `json:"verification_level,omitempty"`
	VerificationPass  *bool               `json:"verification_passed,omitempty"`
	Cleanup           protocol.Cleanup    `json:"cleanup"`
	OutputPath        string              `json:"output_path,omitempty"`
	OutputBytes       int64               `json:"output_bytes"`
	OutputTruncated   bool                `json:"output_truncated"`
	CreatedAt         time.Time           `json:"created_at"`
	// Progress is the live in-flight tally, attached by the task view for a
	// running attempt and nil otherwise (a completed attempt reads its
	// authoritative totals from the columns above).
	Progress *AttemptProgress `json:"progress,omitempty"`
}

const attemptColumns = `id, target_id, worker_id, claim_request_id, executor, model, model_alias, runner, escalated_from, routing, effort, mode, autonomy, worktree_path, branch, base_branch, base_commit, stack_base_commit, head_commit, pid, pid_start, session_id, prompt_version_hash, launches, started_at, finished_at, exit_code, failure_reason, unverified_reason, is_error, result_text, result, num_turns, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd, git_dirty, git_commits, git_files_changed, git_insertions, git_deletions, git_pushed, verification_level, verification_passed, cleanup_outcome, cleanup_reason, cleanup_command, output_path, output_bytes, output_truncated, created_at`

// GetAttempt reads one attempt.
func (s *Store) GetAttempt(ctx context.Context, id string) (*Attempt, error) {
	as, err := scanAttempts(each(s.query(ctx, `SELECT `+attemptColumns+` FROM attempts WHERE id = ?`, id)))
	if err != nil {
		return nil, err
	}
	if len(as) == 0 {
		return nil, fmt.Errorf("attempt %s: %w", id, ErrNotFound)
	}
	return &as[0], nil
}

// GetAttempt is the transactional read.
func (tx *Tx) GetAttempt(ctx context.Context, id string) (*Attempt, error) {
	as, err := scanAttempts(each(tx.Query(ctx, `SELECT `+attemptColumns+` FROM attempts WHERE id = ?`, id)))
	if err != nil {
		return nil, err
	}
	if len(as) == 0 {
		return nil, fmt.Errorf("attempt %s: %w", id, ErrNotFound)
	}
	return &as[0], nil
}

// AttemptsForTarget returns a Target's attempts, oldest first.
func (s *Store) AttemptsForTarget(ctx context.Context, targetID string) ([]Attempt, error) {
	return scanAttempts(each(s.query(ctx, `SELECT `+attemptColumns+` FROM attempts WHERE target_id = ? ORDER BY created_at`, targetID)))
}

// AttemptsForTargets fetches attempts for many Targets in one query.
func (s *Store) AttemptsForTargets(ctx context.Context, targetIDs []string) (map[string][]Attempt, error) {
	out := map[string][]Attempt{}
	if len(targetIDs) == 0 {
		return out, nil
	}
	q, args := inClause(`SELECT `+attemptColumns+` FROM attempts WHERE target_id IN (`, targetIDs, `) ORDER BY created_at`)
	as, err := scanAttempts(each(s.query(ctx, q, args...)))
	if err != nil {
		return nil, err
	}
	for _, a := range as {
		out[a.TargetID] = append(out[a.TargetID], a)
	}
	return out, nil
}

func (tx *Tx) attemptByClaimRequest(ctx context.Context, claimRequestID string) (*Attempt, error) {
	as, err := scanAttempts(each(tx.Query(ctx, `SELECT `+attemptColumns+` FROM attempts WHERE claim_request_id = ?`, claimRequestID)))
	if err != nil {
		return nil, err
	}
	if len(as) == 0 {
		return nil, nil
	}
	return &as[0], nil
}

func (tx *Tx) attemptForTarget(ctx context.Context, targetID string) (*Attempt, error) {
	// Only an UNFINISHED attempt is rebindable (a waiting_human resume: the
	// answer clears finished_at). A finished attempt stays finished — a
	// retried target gets a fresh attempt (M11: new attempt, same target).
	as, err := scanAttempts(each(tx.Query(ctx, `SELECT `+attemptColumns+` FROM attempts WHERE target_id = ? AND finished_at IS NULL ORDER BY created_at DESC LIMIT 1`, targetID)))
	if err != nil {
		return nil, err
	}
	if len(as) == 0 {
		return nil, nil
	}
	return &as[0], nil
}

// AttemptForTarget returns the latest attempt of a Target, or nil.
func (s *Store) AttemptForTarget(ctx context.Context, targetID string) (*Attempt, error) {
	as, err := scanAttempts(each(s.query(ctx, `SELECT `+attemptColumns+` FROM attempts WHERE target_id = ? ORDER BY created_at DESC LIMIT 1`, targetID)))
	if err != nil {
		return nil, err
	}
	if len(as) == 0 {
		return nil, nil
	}
	return &as[0], nil
}

// CheckMCPToken reports whether token belongs to attempt id.
func (s *Store) CheckMCPToken(ctx context.Context, attemptID, token string) (bool, error) {
	var hash string
	err := s.queryRow(ctx, `SELECT mcp_token_hash FROM attempts WHERE id = ?`, attemptID).Scan(&hash)
	if isNoRows(err) {
		return false, nil
	}
	if err != nil {
		return false, fmt.Errorf("read mcp token of %s: %w", attemptID, err)
	}
	return hash == HashToken(token), nil
}

// RecordHeartbeat stores what a heartbeat reports about the attempt (paths,
// pid, session, prompt version) and moves the Target to the reported state.
func (tx *Tx) RecordHeartbeat(ctx context.Context, attemptID string, hb protocol.HeartbeatRequest) (*Target, error) {
	a, err := tx.GetAttempt(ctx, attemptID)
	if err != nil {
		return nil, err
	}
	t, err := tx.checkLease(ctx, a.TargetID, hb.LeaseToken)
	if err != nil {
		return nil, err
	}
	sets, args := []string{"updated_at = ?"}, []any{formatTime(tx.now)}
	add := func(col string, v any) { sets = append(sets, col+" = ?"); args = append(args, v) }
	if hb.Worktree != "" {
		add("worktree_path", hb.Worktree)
	}
	if hb.Branch != "" {
		add("branch", hb.Branch)
	}
	if hb.BaseBranch != "" {
		add("base_branch", hb.BaseBranch)
	}
	if hb.BaseCommit != "" {
		add("base_commit", hb.BaseCommit)
	}
	if hb.PID != 0 {
		add("pid", hb.PID)
		add("pid_start", hb.PIDStart)
	}
	if hb.SessionID != "" {
		add("session_id", hb.SessionID)
	}
	if hb.PromptVersion != nil {
		if err := tx.UpsertPromptVersion(ctx, *hb.PromptVersion); err != nil {
			return nil, err
		}
		add("prompt_version_hash", hb.PromptVersion.Hash)
	}
	if hb.State == model.Running && a.StartedAt.IsZero() {
		add("started_at", formatTime(tx.now))
	}
	args = append(args, attemptID)
	if _, err := tx.Exec(ctx, `UPDATE attempts SET `+joinSets(sets)+` WHERE id = ?`, args...); err != nil {
		return nil, fmt.Errorf("record heartbeat for %s: %w", attemptID, err)
	}
	if len(hb.ForgeToml) > 0 {
		if _, err := tx.Exec(ctx, `UPDATE repositories SET forge_toml = ?, updated_at = ? WHERE name = ?`, string(hb.ForgeToml), formatTime(tx.now), t.Repository); err != nil {
			return nil, fmt.Errorf("record forge.toml for %s: %w", t.Repository, err)
		}
	}
	if hb.State != "" && hb.State != t.State {
		if hb.State == model.Running {
			if _, err := tx.Exec(ctx, `UPDATE attempts SET launches = launches + 1 WHERE id = ?`, attemptID); err != nil {
				return nil, fmt.Errorf("count launch of %s: %w", attemptID, err)
			}
		}
		return tx.Transition(ctx, t.ID, hb.State, TransitionOptions{Actor: a.WorkerID})
	}
	return t, nil
}

// CompleteOutcome is what Complete decided.
type CompleteOutcome struct {
	Target *Target
	Late   bool // the sweeper had already closed the Target; only git/cleanup were recorded
	Again  bool // an idempotent retry; nothing changed
}

// Complete records the worker's terminal report and runs the Target's
// transitions: waiting_human (an open Question), verifying → succeeded |
// unverified (the worker decided L0/L1; L2/L3 leave it in verifying), failed,
// or cancelled. A late completion for a swept Target updates only the git and
// cleanup columns. A retry for an already-finished attempt with the same lease
// token is a no-op that returns the stored outcome.
func (tx *Tx) Complete(ctx context.Context, attemptID string, req protocol.CompleteRequest, requiredLevel int) (*CompleteOutcome, error) {
	a, err := tx.GetAttempt(ctx, attemptID)
	if err != nil {
		return nil, err
	}
	t, err := tx.GetTarget(ctx, a.TargetID)
	if err != nil {
		return nil, err
	}
	var hash sql.NullString
	if err := tx.QueryRow(ctx, `SELECT lease_token_hash FROM targets WHERE id = ?`, t.ID).Scan(&hash); err != nil {
		return nil, fmt.Errorf("read lease of %s: %w", t.ID, err)
	}
	if !a.FinishedAt.IsZero() {
		if hash.Valid && hash.String != HashToken(req.LeaseToken) {
			return nil, fmt.Errorf("attempt %s: %w", attemptID, ErrLease)
		}
		return &CompleteOutcome{Target: t, Again: true}, nil
	}
	late := !model.Leased(t.State)
	if !late && (!hash.Valid || hash.String != HashToken(req.LeaseToken)) {
		return nil, fmt.Errorf("attempt %s: %w", attemptID, ErrLease)
	}
	if err := tx.storeCompletion(ctx, attemptID, req); err != nil {
		return nil, err
	}
	if late {
		if err := tx.Journal(ctx, "attempt.late_completion", EntityAttempt, attemptID, map[string]any{"state": req.State}); err != nil {
			return nil, err
		}
		return &CompleteOutcome{Target: t, Late: true}, nil
	}
	if req.Cleanup.Outcome == "retained" {
		if _, err := tx.Exec(ctx, `UPDATE targets SET retained = 1 WHERE id = ?`, t.ID); err != nil {
			return nil, fmt.Errorf("mark retained: %w", err)
		}
	}
	actor := a.WorkerID
	switch req.State {
	case model.WaitingHuman:
		if req.Question == nil {
			return nil, fmt.Errorf("waiting_human without a question")
		}
		if _, err := tx.CreateQuestion(ctx, a, *req.Question); err != nil {
			return nil, err
		}
		if _, err := tx.Exec(ctx, `UPDATE attempts SET finished_at = NULL WHERE id = ?`, attemptID); err != nil {
			return nil, fmt.Errorf("keep attempt %s open: %w", attemptID, err)
		}
		t, err = tx.Transition(ctx, t.ID, model.WaitingHuman, TransitionOptions{Actor: actor})
	case model.Failed, model.Cancelled:
		t, err = tx.Transition(ctx, t.ID, req.State, TransitionOptions{Reason: req.FailureReason, Actor: actor, Retained: req.Cleanup.Outcome == "retained"})
	case model.Succeeded:
		if t, err = tx.Transition(ctx, t.ID, model.Verifying, TransitionOptions{Actor: actor}); err != nil {
			return nil, err
		}
		switch {
		case !req.Verification.Passed:
			t, err = tx.Transition(ctx, t.ID, model.Unverified, TransitionOptions{UnverifiedReason: req.Verification.Reason, Actor: actor, Retained: req.Cleanup.Outcome == "retained"})
		case req.Verification.Level >= requiredLevel:
			t, err = tx.Transition(ctx, t.ID, model.Succeeded, TransitionOptions{Actor: actor, Retained: req.Cleanup.Outcome == "retained"})
		default:
			// L2/L3 still to come: the Target waits in verifying without a lease.
		}
	default:
		return nil, fmt.Errorf("complete: unexpected state %q", req.State)
	}
	if err != nil {
		return nil, err
	}
	return &CompleteOutcome{Target: t}, nil
}

func (tx *Tx) storeCompletion(ctx context.Context, attemptID string, req protocol.CompleteRequest) error {
	var level, passed any
	if req.Verification.Level > 0 || req.State == model.Succeeded {
		level, passed = req.Verification.Level, boolInt(req.Verification.Passed)
	}
	_, err := tx.Exec(ctx, `UPDATE attempts SET finished_at = ?, exit_code = ?, failure_reason = ?, unverified_reason = ?, is_error = ?, result_text = ?, result = ?, num_turns = ?, input_tokens = ?, output_tokens = ?, cache_read_tokens = ?, cache_creation_tokens = ?, cost_usd = ?, session_id = COALESCE(?, session_id), launches = ?, head_commit = ?, git_dirty = ?, git_commits = ?, git_files_changed = ?, git_insertions = ?, git_deletions = ?, git_pushed = ?, verification_level = ?, verification_passed = ?, cleanup_outcome = ?, cleanup_reason = ?, cleanup_command = ?, output_path = ?, output_bytes = ?, output_truncated = ?, started_at = COALESCE(started_at, ?), updated_at = ? WHERE id = ?`,
		formatTime(req.FinishedAt.UTC()), req.ExitCode, nullString(string(req.FailureReason)), nullString(req.Verification.Reason), boolInt(req.IsError), nullString(truncate(req.ResultText, protocol.MaxResultTextBytes)), jsonRaw(req.Result), req.NumTurns,
		req.Usage.InputTokens, req.Usage.OutputTokens, req.Usage.CacheReadTokens, req.Usage.CacheCreationTokens, nullFloatPtr(req.CostUSD), nullString(req.SessionID), req.Launches,
		nullString(req.Git.Head), boolInt(req.Git.Dirty), req.Git.Commits, req.Git.FilesChanged, req.Git.Insertions, req.Git.Deletions, boolInt(req.Git.Pushed),
		level, passed, req.Cleanup.Outcome, req.Cleanup.Reason, nullString(req.Cleanup.Command), nullString(req.OutputPath), req.OutputBytes, boolInt(req.OutputTruncated), nullTime(req.StartedAt), formatTime(tx.now), attemptID)
	if err != nil {
		return fmt.Errorf("store completion of %s: %w", attemptID, err)
	}
	return tx.Journal(ctx, "attempt.completed", EntityAttempt, attemptID, map[string]any{"state": req.State, "exit_code": req.ExitCode, "reason": req.FailureReason, "cleanup": req.Cleanup.Outcome})
}

// PatchCleanup updates only git and cleanup columns (reconcile after a restart).
func (tx *Tx) PatchCleanup(ctx context.Context, attemptID string, p protocol.CleanupPatch) error {
	sets, args := []string{"cleanup_outcome = ?", "cleanup_reason = ?", "cleanup_command = ?", "updated_at = ?"}, []any{p.Cleanup.Outcome, p.Cleanup.Reason, nullString(p.Cleanup.Command), formatTime(tx.now)}
	if p.Git != nil {
		sets = append(sets, "git_dirty = ?", "git_commits = ?", "git_pushed = ?", "head_commit = ?")
		args = append(args, boolInt(p.Git.Dirty), p.Git.Commits, boolInt(p.Git.Pushed), nullString(p.Git.Head))
	}
	args = append(args, attemptID)
	res, err := tx.Exec(ctx, `UPDATE attempts SET `+joinSets(sets)+` WHERE id = ?`, args...)
	if err != nil {
		return fmt.Errorf("patch cleanup of %s: %w", attemptID, err)
	}
	if err := oneRow(res, "attempt "+attemptID); err != nil {
		return err
	}
	if p.Cleanup.Outcome == "retained" {
		if _, err := tx.Exec(ctx, `UPDATE targets SET retained = 1 WHERE id = (SELECT target_id FROM attempts WHERE id = ?)`, attemptID); err != nil {
			return fmt.Errorf("mark retained: %w", err)
		}
	}
	return tx.Journal(ctx, "attempt.cleanup", EntityAttempt, attemptID, p.Cleanup)
}

func joinSets(sets []string) string {
	out := ""
	for i, s := range sets {
		if i > 0 {
			out += ", "
		}
		out += s
	}
	return out
}

func truncate(s string, n int) string {
	if len(s) <= n {
		return s
	}
	return s[:n]
}

func scanAttempts(iter func(func(*sql.Rows) error) error) ([]Attempt, error) {
	var out []Attempt
	err := iter(func(rows *sql.Rows) error {
		var a Attempt
		var effort, worktree, branch, baseBranch, baseCommit, stackBase, head, session, promptHash, started, finished, failure, unverified, resultText, result, cleanupOutcome, cleanupReason, cleanupCommand, outputPath sql.NullString
		var runner, escalatedFrom, routing sql.NullString
		var pid, pidStart, exitCode, numTurns, in, outT, cacheR, cacheC, dirty, commits, files, ins, del, pushed, vlevel, vpassed, outputBytes, outputTruncated, isError sql.NullInt64
		var cost sql.NullFloat64
		var created string
		if err := rows.Scan(&a.ID, &a.TargetID, &a.WorkerID, &a.ClaimRequestID, &a.Executor, &a.Model, &a.ModelAlias, &runner, &escalatedFrom, &routing, &effort, &a.Mode, &a.Autonomy, &worktree, &branch, &baseBranch, &baseCommit, &stackBase, &head, &pid, &pidStart, &session, &promptHash, &a.Launches, &started, &finished, &exitCode, &failure, &unverified, &isError, &resultText, &result, &numTurns, &in, &outT, &cacheR, &cacheC, &cost, &dirty, &commits, &files, &ins, &del, &pushed, &vlevel, &vpassed, &cleanupOutcome, &cleanupReason, &cleanupCommand, &outputPath, &outputBytes, &outputTruncated, &created); err != nil {
			return fmt.Errorf("scan attempt: %w", err)
		}
		a.Runner, a.EscalatedFrom = runner.String, escalatedFrom.String
		if routing.Valid {
			a.Routing = json.RawMessage(routing.String)
		}
		a.Effort, a.WorktreePath, a.Branch, a.BaseBranch, a.BaseCommit, a.HeadCommit = effort.String, worktree.String, branch.String, baseBranch.String, baseCommit.String, head.String
		a.StackBaseCommit = stackBase.String
		a.SessionID, a.PromptVersionHash, a.UnverifiedReason, a.ResultText, a.OutputPath = session.String, promptHash.String, unverified.String, resultText.String, outputPath.String
		a.FailureReason = model.FailureReason(failure.String)
		a.PID, a.PIDStart, a.NumTurns, a.OutputBytes = int(pid.Int64), pidStart.Int64, int(numTurns.Int64), outputBytes.Int64
		a.IsError, a.OutputTruncated = isError.Int64 == 1, outputTruncated.Int64 == 1
		if exitCode.Valid {
			v := int(exitCode.Int64)
			a.ExitCode = &v
		}
		if result.Valid {
			a.Result = json.RawMessage(result.String)
		}
		a.Usage = protocol.Usage{InputTokens: in.Int64, OutputTokens: outT.Int64, CacheReadTokens: cacheR.Int64, CacheCreationTokens: cacheC.Int64}
		if cost.Valid {
			a.CostUSD = &cost.Float64
		}
		a.Git = protocol.GitOutcome{Dirty: dirty.Int64 == 1, Commits: int(commits.Int64), FilesChanged: int(files.Int64), Insertions: int(ins.Int64), Deletions: int(del.Int64), Pushed: pushed.Int64 == 1, Head: head.String}
		if vlevel.Valid {
			v := int(vlevel.Int64)
			a.VerificationLevel = &v
		}
		if vpassed.Valid {
			v := vpassed.Int64 == 1
			a.VerificationPass = &v
		}
		a.Cleanup = protocol.Cleanup{Outcome: cleanupOutcome.String, Reason: cleanupReason.String, Command: cleanupCommand.String}
		var err error
		if a.StartedAt, err = parseTime(started); err != nil {
			return err
		}
		if a.FinishedAt, err = parseTime(finished); err != nil {
			return err
		}
		if a.CreatedAt, err = parseTime(sql.NullString{String: created, Valid: true}); err != nil {
			return err
		}
		out = append(out, a)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read attempts: %w", err)
	}
	return out, nil
}

// InFlightByRunner counts non-terminal attempts grouped by their runner — the
// M10 runner-capacity denominator (DESIGN.md §21). An attempt is in flight
// while its finished_at is NULL; runs inside the claim transaction so the
// count is race-free against concurrent claims. Attempts with no runner
// (M1-style claims) are not counted.
func (tx *Tx) InFlightByRunner(ctx context.Context) (map[string]int, error) {
	out := map[string]int{}
	err := each(tx.Query(ctx, `SELECT runner, count(*) FROM attempts WHERE finished_at IS NULL AND runner IS NOT NULL AND runner != '' GROUP BY runner`))(func(rows *sql.Rows) error {
		var runner string
		var n int
		if err := rows.Scan(&runner, &n); err != nil {
			return fmt.Errorf("scan in-flight runner: %w", err)
		}
		out[runner] = n
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("count in-flight by runner: %w", err)
	}
	return out, nil
}
