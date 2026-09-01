-- The extension ledger for supervisor adjudication (NOTES.md "Supervisor
-- adjudication seam"): every budget negotiation on a running attempt — a child
-- calling forge_request_budget as it nears a soft phase budget, or the
-- supervisor watchdog acting on silence/spin/cliff — lands one row here, in
-- full, and the whole ledger is handed to every adjudicate() call so that
-- "asked 3x, files unchanged since the first ask" is a decision the policy can
-- see and act on (kill early on diminishing returns).
--
--   seq                per-attempt request counter, 1-based, ordering the ledger.
--   dimension          turns | seconds | tokens | usd — what was asked for.
--   amount             how much was asked.
--   reason             the child's stated reason (untrusted text), or the
--                      watchdog trigger for a supervisor-initiated decision.
--   decision           the adjudicated action: continue | extend | kill.
--   granted_amount     amount actually granted (0 for continue/kill/deny).
--   progress_snapshot  JSON snapshot of the live metrics AT THE TIME of this
--                      request (running_turns, artifact_growth, tokens): the
--                      basis for the diminishing-returns comparison.
--   decided_by         policy | auto:<model> — who made the call.
--   rationale          one-line justification, journaled alongside.
--   at                 when the decision was recorded.
CREATE TABLE attempt_budget_requests (
    id                TEXT PRIMARY KEY,
    attempt_id        TEXT NOT NULL REFERENCES attempts(id),
    seq               INTEGER NOT NULL,
    dimension         TEXT NOT NULL,
    amount            REAL NOT NULL,
    reason            TEXT NOT NULL,
    decision          TEXT NOT NULL,
    granted_amount    REAL NOT NULL DEFAULT 0,
    progress_snapshot TEXT,
    decided_by        TEXT NOT NULL,
    rationale         TEXT,
    at                TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_budget_requests_attempt_seq ON attempt_budget_requests(attempt_id, seq);
