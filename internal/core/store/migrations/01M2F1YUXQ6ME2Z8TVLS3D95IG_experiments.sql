-- Optimization experiments: the LLM-assisted iterate loop, generic over what
-- is being optimized. One row per experiment: an optimizer model proposes
-- variants of a subject (persona:<name> or routine:<name> today;
-- workflow:<name> is the designed next kind), each variant plus the
-- unchanged baseline runs the operator's test on the target model, and the
-- optimizer judges the outputs against the goal. `test` is a JSON spec whose
-- shape belongs to the subject kind. Results are JSON for the UI; nothing
-- applies automatically — a winning variant goes through the subject's
-- ordinary validated edit path when the human clicks Apply.

CREATE TABLE experiments (
    id              TEXT PRIMARY KEY,
    subject         TEXT NOT NULL,      -- <kind>:<name>
    goal            TEXT NOT NULL,
    target_model    TEXT NOT NULL,      -- where the subject should perform
    optimizer_model TEXT NOT NULL,      -- who proposes and judges
    test            TEXT,               -- JSON, subject-kind-specific
    variant_count   INTEGER NOT NULL,
    status          TEXT NOT NULL,      -- running | done | failed
    progress        TEXT,               -- one human-readable line
    baseline        TEXT,               -- the content being varied
    results         TEXT,               -- JSON experimentResults
    error           TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE INDEX idx_experiments_subject ON experiments (subject, created_at);
