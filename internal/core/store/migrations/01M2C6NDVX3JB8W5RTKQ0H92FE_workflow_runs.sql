-- First-class workflow runs: the run engine's durable state. The graph is
-- frozen at run creation (byte-for-byte what routes, like a Work's routine
-- snapshot); node execution state lives in workflow_run_nodes, one row per
-- node *instance* (a loop re-entry is a fresh instance at iteration+1). A
-- routine instance's live progress is still derived from its Work — the row
-- records engine decisions (readiness, tokens, outputs, skips), which are
-- events, not derivations. Runs with no row here are legacy (pre-engine,
-- fully instantiated upfront) and stay readable through the derived path.

CREATE TABLE workflow_runs (
    id                  TEXT PRIMARY KEY,
    workflow_id         TEXT NOT NULL,
    workflow_name       TEXT NOT NULL,
    workflow_generation INTEGER NOT NULL,
    graph               TEXT NOT NULL,             -- frozen JSON WorkflowGraph
    context             TEXT NOT NULL,             -- JSON {repositories, objective}
    status              TEXT NOT NULL,             -- running|succeeded|failed|cancelled|partial
    trigger             TEXT NOT NULL,             -- manual|schedule
    script_runs         INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    finished_at         TEXT
);

CREATE INDEX idx_workflow_runs_open ON workflow_runs (status) WHERE status = 'running';
CREATE INDEX idx_workflow_runs_name ON workflow_runs (workflow_name, created_at);

CREATE TABLE workflow_run_nodes (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES workflow_runs(id),
    node_id     TEXT NOT NULL,
    iteration   INTEGER NOT NULL DEFAULT 1,
    type        TEXT NOT NULL,
    status      TEXT NOT NULL,                     -- pending|ready|running|succeeded|failed|skipped|cancelled
    work_id     TEXT,
    edges       TEXT NOT NULL DEFAULT '{}',        -- JSON: incoming edge key -> taken|dead
    output      TEXT,                              -- JSON node output, size-capped
    error       TEXT,
    started_at  TEXT,
    finished_at TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (run_id, node_id, iteration)
);

CREATE INDEX idx_run_nodes_run ON workflow_run_nodes (run_id);
CREATE INDEX idx_run_nodes_work ON workflow_run_nodes (work_id) WHERE work_id IS NOT NULL;
