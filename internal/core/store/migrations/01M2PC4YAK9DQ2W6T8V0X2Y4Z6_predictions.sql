-- The prediction ledger: every consequential judgment forge makes is a
-- prediction that later resolves (an applied edit survives the A/B net or is
-- reverted; a promoted experiment arm holds up). Recording and resolving
-- them yields per-source calibration — the objective trust measure behind
-- the autonomy ladder.
CREATE TABLE predictions (
    id          TEXT PRIMARY KEY,
    source      TEXT NOT NULL,             -- 'experiment' | 'proposal' | agent-stated source
    source_ref  TEXT NOT NULL,             -- experiment id, work id, …
    proposal_id TEXT NOT NULL DEFAULT '',  -- resolution handle when tied to a proposal
    subject     TEXT NOT NULL,             -- directive:x, persona:y, workflow:z
    statement   TEXT NOT NULL,
    probability REAL,                      -- stated confidence; NULL when unstated
    created_at  TEXT NOT NULL,
    resolve_by  TEXT NOT NULL,
    resolved_at TEXT,
    outcome     INTEGER,                   -- 1 held / 0 failed; NULL = unresolvable
    note        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_predictions_open ON predictions(resolve_by) WHERE resolved_at IS NULL;
CREATE INDEX idx_predictions_source ON predictions(source, created_at);
