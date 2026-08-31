-- Workflows: routines strung together. A workflow is a definition-layer object;
-- running one instantiates one Work per step with blocked_by edges, so the
-- existing queue, dependency, and attention machinery applies unchanged. A run
-- has no state of its own: it is the derived states of its Works, grouped by
-- workflow_run_id.

CREATE TABLE workflows (
    id               TEXT PRIMARY KEY,
    name             TEXT NOT NULL UNIQUE,
    steps            TEXT NOT NULL,            -- JSON array of {name, routine, after}
    schedule         TEXT,
    schedule_enabled INTEGER NOT NULL DEFAULT 0,
    generation       INTEGER NOT NULL DEFAULT 1,
    next_due_at      TEXT,
    archived_at      TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);

CREATE TABLE workflow_generations (
    workflow_id TEXT NOT NULL REFERENCES workflows(id),
    generation  INTEGER NOT NULL,
    snapshot    TEXT NOT NULL,                 -- JSON
    source      TEXT NOT NULL,                 -- edit | proposal:<id>
    created_at  TEXT NOT NULL,
    PRIMARY KEY (workflow_id, generation)
);

ALTER TABLE work ADD COLUMN workflow_run_id TEXT;
ALTER TABLE work ADD COLUMN workflow_name TEXT;
ALTER TABLE work ADD COLUMN workflow_step TEXT;

CREATE INDEX idx_work_workflow_run ON work (workflow_run_id) WHERE workflow_run_id IS NOT NULL;
