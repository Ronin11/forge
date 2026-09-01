package store

// The extension ledger and the live signals the supervisor adjudicator reads
// (NOTES.md "Supervisor adjudication seam"). Every budget negotiation on a
// running attempt is journaled here in full; the adjudicator also reads the
// live per-attempt signals (turns, artifact growth, tool-signature dominance)
// derived from the events table, so it never recomputes what attempt_progress
// already tracks and never trusts anything but the events it ingested.

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"time"

	"forge/internal/model"
)

// Budget dimensions a request may ask along. Kept here so the tool, the
// adjudicator, and the ledger agree on one vocabulary.
const (
	BudgetTurns   = "turns"
	BudgetSeconds = "seconds"
	BudgetTokens  = "tokens"
	BudgetUSD     = "usd"
)

// Adjudicated actions recorded as a ledger row's decision.
const (
	BudgetContinue = "continue" // no new budget, keep running
	BudgetExtend   = "extend"   // grant granted_amount along dimension
	BudgetKill     = "kill"     // reap the attempt (shadow mode may not enforce)
)

// BudgetRequest is one row of the extension ledger: a request and the decision
// it drew, with the progress snapshot that decision was made against.
type BudgetRequest struct {
	ID               string          `json:"id"`
	AttemptID        string          `json:"attempt_id"`
	Seq              int             `json:"seq"`
	Dimension        string          `json:"dimension"`
	Amount           float64         `json:"amount"`
	Reason           string          `json:"reason"`
	Decision         string          `json:"decision"`
	GrantedAmount    float64         `json:"granted_amount"`
	ProgressSnapshot json.RawMessage `json:"progress_snapshot,omitempty"`
	DecidedBy        string          `json:"decided_by"`
	Rationale        string          `json:"rationale"`
	At               time.Time       `json:"at"`
}

// AppendBudgetRequest records one adjudicated request, assigning the next
// per-attempt seq inside the same transaction so the ledger is gap-free and
// totally ordered. The returned row carries the assigned id/seq/at.
func (tx *Tx) AppendBudgetRequest(ctx context.Context, r BudgetRequest) (BudgetRequest, error) {
	var next int
	if err := tx.QueryRow(ctx, `SELECT COALESCE(MAX(seq), 0) + 1 FROM attempt_budget_requests WHERE attempt_id = ?`, r.AttemptID).Scan(&next); err != nil {
		return BudgetRequest{}, fmt.Errorf("next budget seq for %s: %w", r.AttemptID, err)
	}
	r.ID, r.Seq, r.At = model.NewID(), next, tx.now
	var snap any
	if len(r.ProgressSnapshot) > 0 {
		snap = string(r.ProgressSnapshot)
	}
	_, err := tx.Exec(ctx, `INSERT INTO attempt_budget_requests
		(id, attempt_id, seq, dimension, amount, reason, decision, granted_amount, progress_snapshot, decided_by, rationale, at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		r.ID, r.AttemptID, r.Seq, r.Dimension, r.Amount, r.Reason, r.Decision, r.GrantedAmount, snap, r.DecidedBy, nullString(r.Rationale), formatTime(r.At))
	if err != nil {
		return BudgetRequest{}, fmt.Errorf("append budget request for %s: %w", r.AttemptID, err)
	}
	return r, nil
}

// BudgetLedger returns the full request history of an attempt, ordered by seq —
// the whole thing is handed to every adjudicate() call.
func (s *Store) BudgetLedger(ctx context.Context, attemptID string) ([]BudgetRequest, error) {
	return scanBudgetLedger(func(scan func(*sql.Rows) error) error {
		return each(s.query(ctx, budgetLedgerSelect, attemptID))(scan)
	})
}

// BudgetLedger (Tx) is the in-transaction read, so an adjudicate + append is
// one atomic step over a consistent ledger.
func (tx *Tx) BudgetLedger(ctx context.Context, attemptID string) ([]BudgetRequest, error) {
	return scanBudgetLedger(func(scan func(*sql.Rows) error) error {
		return each(tx.Query(ctx, budgetLedgerSelect, attemptID))(scan)
	})
}

const budgetLedgerSelect = `SELECT id, attempt_id, seq, dimension, amount, reason, decision,
	granted_amount, progress_snapshot, decided_by, rationale, at
	FROM attempt_budget_requests WHERE attempt_id = ? ORDER BY seq`

func scanBudgetLedger(iter func(func(*sql.Rows) error) error) ([]BudgetRequest, error) {
	var out []BudgetRequest
	err := iter(func(rows *sql.Rows) error {
		var r BudgetRequest
		var snap, rationale, at sql.NullString
		if err := rows.Scan(&r.ID, &r.AttemptID, &r.Seq, &r.Dimension, &r.Amount, &r.Reason,
			&r.Decision, &r.GrantedAmount, &snap, &r.DecidedBy, &rationale, &at); err != nil {
			return fmt.Errorf("scan budget request: %w", err)
		}
		if snap.Valid {
			r.ProgressSnapshot = json.RawMessage(snap.String)
		}
		r.Rationale = rationale.String
		t, err := parseTime(at)
		if err != nil {
			return err
		}
		r.At = t
		out = append(out, r)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read budget ledger: %w", err)
	}
	return out, nil
}

// ArtifactGrowth counts the file-mutating tool calls an attempt has made so far,
// derived from the ingested tool_use spans (Write/Edit/MultiEdit/NotebookEdit).
// It is a deliberately simple, monotonic proxy for "did real work happen": the
// adjudicator snapshots it per request and compares snapshots to detect that no
// artifact has changed since the first ask. Bash-driven commits are not counted
// here (their effect shows up as edits earlier in the run), keeping the signal
// cheap and stable under at-least-once event delivery.
func (s *Store) ArtifactGrowth(ctx context.Context, attemptID string) (int, error) {
	var n int
	err := s.queryRow(ctx, `SELECT COUNT(*) FROM events
		WHERE attempt_id = ? AND kind = ? AND name IN ('Write','Edit','MultiEdit','NotebookEdit')`,
		attemptID, "span_start").Scan(&n)
	if err != nil {
		return 0, fmt.Errorf("artifact growth for %s: %w", attemptID, err)
	}
	return n, nil
}

// ToolSignature is one bucket of the recent tool-use histogram: a tool name
// paired with a short hash of its input summary, and how many of the last K
// calls matched it.
type ToolSignature struct {
	Signature string `json:"signature"`
	Count     int    `json:"count"`
}

// ToolDominance summarizes the tool-signature histogram over the last window
// calls: the share of the window taken by the single most frequent
// tool+args-hash signature (0 when there are no calls), plus that signature and
// the sample size. A dominance near 1 over a real window is the spin signal.
func (s *Store) ToolDominance(ctx context.Context, attemptID string, window int) (dominance float64, top string, samples int, err error) {
	if window < 1 {
		window = 1
	}
	counts := map[string]int{}
	order := []string{}
	ierr := each(s.query(ctx, `SELECT name, COALESCE(json_extract(attrs, '$.input_summary'), '') FROM events
		WHERE attempt_id = ? AND kind = ?
		ORDER BY time DESC, source DESC, seq DESC LIMIT ?`, attemptID, "span_start", window))(func(rows *sql.Rows) error {
		var name, summary string
		if err := rows.Scan(&name, &summary); err != nil {
			return fmt.Errorf("scan tool signature: %w", err)
		}
		sig := toolSignature(name, summary)
		if _, seen := counts[sig]; !seen {
			order = append(order, sig)
		}
		counts[sig]++
		samples++
		return nil
	})
	if ierr != nil {
		return 0, "", 0, fmt.Errorf("tool dominance for %s: %w", attemptID, ierr)
	}
	for _, sig := range order {
		if counts[sig] > 0 && (top == "" || counts[sig] > counts[top]) {
			top = sig
		}
	}
	if samples > 0 && top != "" {
		dominance = float64(counts[top]) / float64(samples)
	}
	return dominance, top, samples, nil
}

// toolSignature is a tool_use fingerprint: the tool name plus a short hash of
// its input summary, so the same tool called with the same arguments collapses
// to one bucket while different arguments spread out.
func toolSignature(name, summary string) string {
	sum := sha256.Sum256([]byte(summary))
	return name + "|" + hex.EncodeToString(sum[:4])
}

// RunningAttempt is the minimal view of an in-flight attempt the watchdog
// sweeps: enough to assemble evidence and, on a kill, reach its target/work.
type RunningAttempt struct {
	ID        string
	TargetID  string
	WorkID    string
	StartedAt time.Time
}

// RunningAttempts lists the attempts whose target is still running (not yet
// finished), oldest first — the sweep set for the supervisor watchdog.
func (s *Store) RunningAttempts(ctx context.Context) ([]RunningAttempt, error) {
	var out []RunningAttempt
	err := each(s.query(ctx, `SELECT a.id, a.target_id, t.work_id, a.started_at
		FROM attempts a JOIN targets t ON t.id = a.target_id
		WHERE a.finished_at IS NULL AND t.state = 'running'
		ORDER BY a.started_at`))(func(rows *sql.Rows) error {
		var ra RunningAttempt
		var started sql.NullString
		if err := rows.Scan(&ra.ID, &ra.TargetID, &ra.WorkID, &started); err != nil {
			return fmt.Errorf("scan running attempt: %w", err)
		}
		t, err := parseTime(started)
		if err != nil {
			return err
		}
		ra.StartedAt = t
		out = append(out, ra)
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("list running attempts: %w", err)
	}
	return out, nil
}

// BudgetOutcome is the child-facing result of a forge_request_budget call: the
// tool returns it verbatim to the agent.
type BudgetOutcome struct {
	Decision      string  `json:"decision"` // granted | denied
	GrantedAmount float64 `json:"granted_amount"`
	Message       string  `json:"message"`
}

// HasJournal reports whether any journal row of the given kind exists for an
// entity — used for once-only actions (e.g. the one-time proactive nudge).
func (tx *Tx) HasJournal(ctx context.Context, entityType, entityID, kind string) (bool, error) {
	var n int
	if err := tx.QueryRow(ctx, `SELECT COUNT(*) FROM journal WHERE entity_type = ? AND entity_id = ? AND kind = ?`,
		entityType, entityID, kind).Scan(&n); err != nil {
		return false, fmt.Errorf("count journal %s for %s: %w", kind, entityID, err)
	}
	return n > 0, nil
}

// RequestTargetCancel flags a running target for cancellation the same way the
// operator cancel path does: the worker sees cancel_requested on its next
// heartbeat and stops, and the target requeues under its retry policy. Returns
// whether a still-running target was flagged.
func (tx *Tx) RequestTargetCancel(ctx context.Context, targetID, actor string) (bool, error) {
	res, err := tx.Exec(ctx, `UPDATE targets SET cancel_requested = 1, updated_at = ? WHERE id = ? AND finished_at IS NULL`,
		formatTime(tx.now), targetID)
	if err != nil {
		return false, fmt.Errorf("request cancel of %s: %w", targetID, err)
	}
	n, err := res.RowsAffected()
	if err != nil {
		return false, fmt.Errorf("cancel rows for %s: %w", targetID, err)
	}
	if n == 0 {
		return false, nil
	}
	if err := tx.Journal(ctx, "target.cancel_requested", EntityTarget, targetID, map[string]string{"actor": actor}); err != nil {
		return false, err
	}
	return true, nil
}

// GrantedBudget is one pending budget grant to actuate on the next heartbeat.
type GrantedBudget struct {
	Dimension string  `json:"dimension"`
	Amount    float64 `json:"amount"`
	Nudge     string  `json:"nudge,omitempty"`
}

// TakeBudgetGrants returns the attempt's un-actuated grants, oldest first, and
// marks them actuated in the same transaction — handing them to the heartbeat
// response is the actuation. It mirrors TakeSteers: the attempt.budget_granted
// journal rows are both the audit trail and the delivery queue, with an
// attempt.budget_actuated watermark row recording how far delivery has reached,
// so a daemon restart never loses or double-delivers a grant.
func (tx *Tx) TakeBudgetGrants(ctx context.Context, attemptID string) ([]GrantedBudget, error) {
	var grants []GrantedBudget
	var lastID int64
	err := each(tx.Query(ctx, `SELECT id, payload FROM journal
		WHERE entity_type = ? AND entity_id = ? AND kind = 'attempt.budget_granted'
		AND id > COALESCE((SELECT max(id) FROM journal WHERE entity_type = ? AND entity_id = ? AND kind = 'attempt.budget_actuated'), 0)
		ORDER BY id`, EntityAttempt, attemptID, EntityAttempt, attemptID))(func(rows *sql.Rows) error {
		var id int64
		var payload string
		if err := rows.Scan(&id, &payload); err != nil {
			return err
		}
		var body struct {
			Dimension string  `json:"dimension"`
			Amount    float64 `json:"granted_amount"`
			Nudge     string  `json:"nudge"`
		}
		if err := json.Unmarshal([]byte(payload), &body); err != nil {
			return fmt.Errorf("budget grant row %d: %w", id, err)
		}
		grants = append(grants, GrantedBudget{Dimension: body.Dimension, Amount: body.Amount, Nudge: body.Nudge})
		lastID = id
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("read pending budget grants of %s: %w", attemptID, err)
	}
	if len(grants) == 0 {
		return nil, nil
	}
	if err := tx.Journal(ctx, "attempt.budget_actuated", EntityAttempt, attemptID, map[string]any{"through": lastID, "count": len(grants)}); err != nil {
		return nil, err
	}
	return grants, nil
}
