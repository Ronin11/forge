-- Attribution keys for the learning loop: which directive and which library
-- version produced each attempt (the post-restructure analog of routine
-- generation — library edits never bump a generation, so before/after
-- comparison needs its own dimension), plus the supervise verdict as data.
ALTER TABLE attempt_facts ADD COLUMN directive TEXT NOT NULL DEFAULT '';
ALTER TABLE attempt_facts ADD COLUMN library_commit TEXT NOT NULL DEFAULT '';
ALTER TABLE attempt_facts ADD COLUMN supervise_outcome TEXT NOT NULL DEFAULT '';
ALTER TABLE attempt_facts ADD COLUMN supervise_round INTEGER;
CREATE INDEX idx_facts_directive ON attempt_facts(directive, finished_at) WHERE directive != '';

-- The declared-and-never-written evals table becomes real per-case history:
-- every golden-eval run keeps its per-case verdicts, so a prompt change's
-- effect on the fixtures is a time series, not one float on a proposal.
DROP TABLE evals;
CREATE TABLE eval_cases (
    id           TEXT PRIMARY KEY,
    proposal_id  TEXT NOT NULL DEFAULT '',  -- '' for ad-hoc `forge eval` runs
    mode         TEXT NOT NULL,
    case_name    TEXT NOT NULL,
    pass         INTEGER NOT NULL,
    state        TEXT NOT NULL,
    failure_reason TEXT NOT NULL DEFAULT '',
    turns        INTEGER,
    cost_usd     REAL,
    details      TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL
);
CREATE INDEX idx_eval_cases_mode ON eval_cases(mode, created_at);
