package store

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"forge/internal/core/model"
)

// LeaseDuration is how long a claim or heartbeat holds a Target; RestartGrace is
// what a starting daemon grants every live lease so a restart never expires
// running attempts (DESIGN.md §14).
const (
	LeaseDuration = 30 * time.Second
	RestartGrace  = 120 * time.Second
)

// ErrLease is wrapped when a lease token does not match or has lapsed.
var ErrLease = errors.New("lease not held")

// HashToken is the one way tokens are stored: never the token itself.
func HashToken(token string) string {
	sum := sha256.Sum256([]byte(token))
	return hex.EncodeToString(sum[:])
}

// TransitionOptions carry what a transition records besides the states.
type TransitionOptions struct {
	Reason           model.FailureReason
	UnverifiedReason string
	Actor            string // worker id, "human", "sweeper", "daemon"
	Retained         bool
}

// Transition moves a Target through model.Transition, stamps the timestamps the
// new state implies, and journals it. It is the only writer of targets.state.
func (tx *Tx) Transition(ctx context.Context, targetID string, to model.State, opts TransitionOptions) (*Target, error) {
	t, err := tx.GetTarget(ctx, targetID)
	if err != nil {
		return nil, err
	}
	w, err := tx.GetWork(ctx, t.WorkID)
	if err != nil {
		return nil, err
	}
	if err := model.Transition(t.State, to, w.Integrate); err != nil {
		return nil, fmt.Errorf("target %s: %w", targetID, err)
	}
	from := t.State
	t.State = to
	now := formatTime(tx.now)
	set := `state = ?, updated_at = ?`
	args := []any{string(to), now}
	switch to {
	case model.Claimed:
		set += `, claimed_at = ?, cancel_requested = 0`
		args = append(args, now)
		if !t.ClaimedAt.IsZero() {
			// A resume: keep the original claimed_at for queue_wait.
			set = `state = ?, updated_at = ?, cancel_requested = 0`
			args = []any{string(to), now}
		}
	case model.Running:
		if t.StartedAt.IsZero() {
			set += `, started_at = ?`
			args = append(args, now)
		}
	case model.Pending, model.WaitingHuman, model.Verifying, model.QueuedForMerge, model.Conflict:
		set += `, worker_id = worker_id, lease_token_hash = NULL, lease_expires_at = NULL`
	}
	if model.IsTerminal(to, w.Integrate) {
		set += `, finished_at = ?, lease_token_hash = NULL, lease_expires_at = NULL`
		args = append(args, now)
	}
	if opts.Reason != "" {
		set += `, failure_reason = ?`
		args = append(args, string(opts.Reason))
		t.FailureReason = opts.Reason
	}
	if opts.UnverifiedReason != "" {
		set += `, unverified_reason = ?`
		args = append(args, opts.UnverifiedReason)
		t.UnverifiedReason = opts.UnverifiedReason
	}
	if opts.Retained {
		set += `, retained = 1`
		t.Retained = true
	}
	args = append(args, targetID)
	if _, err := tx.Exec(ctx, `UPDATE targets SET `+set+` WHERE id = ?`, args...); err != nil {
		return nil, fmt.Errorf("update target %s: %w", targetID, err)
	}
	// The write-set lease (DESIGN.md §10.2, §20) lives exactly as long as the
	// states model.HoldsWriteSet names; a resume re-acquires it at claim.
	if !model.HoldsWriteSet(to) {
		if _, err := tx.Exec(ctx, `DELETE FROM path_leases WHERE target_id = ?`, targetID); err != nil {
			return nil, fmt.Errorf("release path lease of %s: %w", targetID, err)
		}
	}
	if err := tx.Journal(ctx, "target.transition", EntityTarget, targetID, map[string]any{
		"from": from, "to": to, "reason": opts.Reason, "unverified_reason": opts.UnverifiedReason, "actor": opts.Actor, "work_id": t.WorkID,
	}); err != nil {
		return nil, err
	}
	if model.IsTerminal(to, w.Integrate) {
		if err := tx.finishWorkIfDone(ctx, w); err != nil {
			return nil, err
		}
	}
	return t, nil
}

// finishWorkIfDone stamps work.finished_at once every Target is terminal.
func (tx *Tx) finishWorkIfDone(ctx context.Context, w *Work) error {
	var open int
	err := tx.QueryRow(ctx, `SELECT count(*) FROM targets WHERE work_id = ? AND finished_at IS NULL`, w.ID).Scan(&open)
	if err != nil {
		return fmt.Errorf("count open targets of %s: %w", w.ID, err)
	}
	if open > 0 {
		return nil
	}
	return tx.FinishWork(ctx, w.ID)
}

// ClaimParams is what the claim transaction records.
type ClaimParams struct {
	TargetID       string
	WorkerID       string
	ClaimRequestID string
	LeaseToken     string
	MCPToken       string
	Executor       string
	Model          string // resolved id
	ModelAlias     string
	// Runner, EscalatedFrom, and Routing are the M10 claim-time routing record
	// (DESIGN.md §21): the runner the chosen model runs on, the previous
	// attempt's alias when this claim escalated to the next rung, and the JSON
	// routing decision. All empty on an M1-style claim.
	Runner        string
	EscalatedFrom string
	Routing       string // JSON; "" stores NULL
	Effort        string
	Mode          string
	Autonomy      model.Autonomy
	Globs         []string
	// StackBase is the dependency's branch head a stacked attempt starts on
	// (DESIGN.md §20); empty for an unstacked claim.
	StackBase string
}

// Claim moves a pending Target to claimed for one worker and creates (or, on a
// resume, reuses) its Attempt, in the caller's transaction. A repeated
// claim_request_id returns the existing attempt so a lost response is safe.
func (tx *Tx) Claim(ctx context.Context, p ClaimParams) (*Attempt, error) {
	if existing, err := tx.attemptByClaimRequest(ctx, p.ClaimRequestID); err != nil {
		return nil, err
	} else if existing != nil {
		return existing, nil
	}
	t, err := tx.GetTarget(ctx, p.TargetID)
	if err != nil {
		return nil, err
	}
	if t.State != model.Pending {
		return nil, fmt.Errorf("target %s is %s: %w", p.TargetID, t.State, ErrConflict)
	}
	if t.WorkerID != "" && t.WorkerID != p.WorkerID {
		return nil, fmt.Errorf("target %s is pinned to worker %s: %w", p.TargetID, t.WorkerID, ErrConflict)
	}
	if _, err := tx.Transition(ctx, p.TargetID, model.Claimed, TransitionOptions{Actor: p.WorkerID}); err != nil {
		return nil, err
	}
	expires := tx.now.Add(LeaseDuration)
	if _, err := tx.Exec(ctx, `UPDATE targets SET worker_id = ?, lease_token_hash = ?, lease_expires_at = ? WHERE id = ?`,
		p.WorkerID, HashToken(p.LeaseToken), formatTime(expires), p.TargetID); err != nil {
		return nil, fmt.Errorf("lease target %s: %w", p.TargetID, err)
	}
	// The caller passes the effective globs (controlplane.EffectiveGlobs:
	// undeclared paths lease the whole repository as ["**"]); an empty list
	// means the Work's mode is lease-exempt (writes nothing) and no row is
	// taken.
	if len(p.Globs) > 0 {
		if _, err := tx.Exec(ctx, `INSERT OR REPLACE INTO path_leases (target_id, repository_name, globs, acquired_at) VALUES (?, ?, ?, ?)`,
			p.TargetID, t.Repository, jsonList(p.Globs), formatTime(tx.now)); err != nil {
			return nil, fmt.Errorf("path lease for %s: %w", p.TargetID, err)
		}
	}
	// A resumed Target already has its attempt; a new claim gets a new one.
	if a, err := tx.attemptForTarget(ctx, p.TargetID); err != nil {
		return nil, err
	} else if a != nil {
		if _, err := tx.Exec(ctx, `UPDATE attempts SET claim_request_id = ?, mcp_token_hash = ?, updated_at = ? WHERE id = ?`,
			p.ClaimRequestID, HashToken(p.MCPToken), formatTime(tx.now), a.ID); err != nil {
			return nil, fmt.Errorf("rebind attempt %s: %w", a.ID, err)
		}
		a.ClaimRequestID = p.ClaimRequestID
		return a, nil
	}
	a := &Attempt{
		ID: model.NewID(), TargetID: p.TargetID, WorkerID: p.WorkerID, ClaimRequestID: p.ClaimRequestID,
		Executor: p.Executor, Model: p.Model, ModelAlias: p.ModelAlias, Runner: p.Runner, EscalatedFrom: p.EscalatedFrom,
		Effort: p.Effort, Mode: p.Mode, Autonomy: p.Autonomy, StackBaseCommit: p.StackBase, CreatedAt: tx.now,
	}
	if p.Routing != "" {
		a.Routing = json.RawMessage(p.Routing)
	}
	if _, err := tx.Exec(ctx, `INSERT INTO attempts (id, target_id, worker_id, claim_request_id, mcp_token_hash, executor, model, model_alias, runner, escalated_from, routing, effort, mode, autonomy, stack_base_commit, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		a.ID, a.TargetID, a.WorkerID, a.ClaimRequestID, HashToken(p.MCPToken), a.Executor, a.Model, a.ModelAlias, nullString(p.Runner), nullString(p.EscalatedFrom), nullString(p.Routing), nullString(a.Effort), a.Mode, string(a.Autonomy), nullString(a.StackBaseCommit), formatTime(tx.now), formatTime(tx.now)); err != nil {
		return nil, fmt.Errorf("insert attempt: %w", err)
	}
	if err := tx.Journal(ctx, "attempt.created", EntityAttempt, a.ID, map[string]any{"target_id": a.TargetID, "worker_id": a.WorkerID}); err != nil {
		return nil, err
	}
	return a, nil
}

// checkLease verifies a lease token against the Target and reports whether the
// lease is still current. An expired-but-unswept lease is still accepted (the
// sweeper decides expiry, not a race with the heartbeat).
func (tx *Tx) checkLease(ctx context.Context, targetID, token string) (*Target, error) {
	t, err := tx.GetTarget(ctx, targetID)
	if err != nil {
		return nil, err
	}
	var hash sql.NullString
	if err := tx.QueryRow(ctx, `SELECT lease_token_hash FROM targets WHERE id = ?`, targetID).Scan(&hash); err != nil {
		return nil, fmt.Errorf("read lease of %s: %w", targetID, err)
	}
	if !hash.Valid || hash.String != HashToken(token) || !model.Leased(t.State) {
		return nil, fmt.Errorf("target %s: %w", targetID, ErrLease)
	}
	return t, nil
}

// Heartbeat renews the lease and returns whether cancellation was requested.
func (tx *Tx) Heartbeat(ctx context.Context, targetID, token string) (cancel bool, expires time.Time, err error) {
	t, err := tx.checkLease(ctx, targetID, token)
	if err != nil {
		return false, time.Time{}, err
	}
	expires = tx.now.Add(LeaseDuration)
	if _, err := tx.Exec(ctx, `UPDATE targets SET lease_expires_at = ?, updated_at = ? WHERE id = ?`, formatTime(expires), formatTime(tx.now), targetID); err != nil {
		return false, time.Time{}, fmt.Errorf("renew lease of %s: %w", targetID, err)
	}
	return t.CancelRequested, expires, nil
}

// SweepExpiredLeases fails every leased Target whose lease lapsed and returns
// the affected Targets so facts can be computed. Merging Targets go back to the
// merge queue instead (DESIGN.md §4.1).
func (tx *Tx) SweepExpiredLeases(ctx context.Context) ([]Target, error) {
	expired, err := scanTargets(each(tx.Query(ctx, `SELECT `+targetColumns+` FROM targets WHERE lease_expires_at IS NOT NULL AND lease_expires_at < ? AND state IN ('claimed','preparing','running','merging')`, formatTime(tx.now))))
	if err != nil {
		return nil, err
	}
	var out []Target
	for _, t := range expired {
		to, opts := model.Failed, TransitionOptions{Reason: model.ReasonLeaseExpired, Actor: "sweeper"}
		if t.State == model.Merging {
			to, opts = model.QueuedForMerge, TransitionOptions{Actor: "sweeper"}
		}
		moved, err := tx.Transition(ctx, t.ID, to, opts)
		if err != nil {
			return nil, err
		}
		if to == model.Failed {
			if _, err := tx.Exec(ctx, `UPDATE attempts SET finished_at = ?, failure_reason = ?, updated_at = ? WHERE target_id = ? AND finished_at IS NULL`,
				formatTime(tx.now), string(model.ReasonLeaseExpired), formatTime(tx.now), t.ID); err != nil {
				return nil, fmt.Errorf("close attempt of %s: %w", t.ID, err)
			}
		}
		out = append(out, *moved)
	}
	return out, nil
}

// ExtendLeases grants every leased Target a fresh lease of RestartGrace — what a
// starting daemon does before its sweeper's first tick.
func (tx *Tx) ExtendLeases(ctx context.Context) (int, error) {
	res, err := tx.Exec(ctx, `UPDATE targets SET lease_expires_at = ? WHERE state IN ('claimed','preparing','running','merging') AND lease_token_hash IS NOT NULL`, formatTime(tx.now.Add(RestartGrace)))
	if err != nil {
		return 0, fmt.Errorf("extend leases: %w", err)
	}
	n, err := res.RowsAffected()
	if err != nil {
		return 0, fmt.Errorf("extend leases: %w", err)
	}
	if n > 0 {
		if err := tx.Journal(ctx, "daemon.leases_extended", EntityDaemon, "daemon", map[string]any{"targets": n, "until": tx.now.Add(RestartGrace)}); err != nil {
			return 0, err
		}
	}
	return int(n), nil
}

// CancelWork cancels every non-terminal Target: those without a lease
// immediately, leased ones by request (the worker sees it on its heartbeat).
func (tx *Tx) CancelWork(ctx context.Context, workID, actor string) error {
	targets, err := scanTargets(each(tx.Query(ctx, `SELECT `+targetColumns+` FROM targets WHERE work_id = ? AND finished_at IS NULL`, workID)))
	if err != nil {
		return err
	}
	if len(targets) == 0 {
		w, err := tx.GetWork(ctx, workID)
		if err != nil {
			return err
		}
		if !w.FinishedAt.IsZero() {
			return fmt.Errorf("work %s already finished: %w", workID, ErrConflict)
		}
	}
	for _, t := range targets {
		if model.Leased(t.State) {
			if _, err := tx.Exec(ctx, `UPDATE targets SET cancel_requested = 1, updated_at = ? WHERE id = ?`, formatTime(tx.now), t.ID); err != nil {
				return fmt.Errorf("request cancel of %s: %w", t.ID, err)
			}
			if err := tx.Journal(ctx, "target.cancel_requested", EntityTarget, t.ID, map[string]string{"actor": actor}); err != nil {
				return err
			}
			continue
		}
		if _, err := tx.Transition(ctx, t.ID, model.Cancelled, TransitionOptions{Reason: model.ReasonCancelled, Actor: actor}); err != nil {
			return err
		}
	}
	return nil
}
