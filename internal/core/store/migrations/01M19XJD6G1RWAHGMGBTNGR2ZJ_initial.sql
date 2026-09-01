-- Forge schema. Every table exists from this migration, including those unused
-- until later milestones (DESIGN.md §3). Timestamps are RFC 3339 UTC text; JSON is
-- text; booleans are 0/1 integers.

CREATE TABLE projects (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL UNIQUE,
    autonomy          TEXT,
    budget_class      TEXT NOT NULL DEFAULT 'normal',
    priority_baseline INTEGER NOT NULL DEFAULT 50,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

CREATE TABLE repositories (
    name            TEXT PRIMARY KEY,
    project_id      TEXT NOT NULL REFERENCES projects(id),
    path            TEXT NOT NULL,
    origin_identity TEXT NOT NULL,
    base_branch     TEXT,
    worker_id       TEXT,
    forge_toml      TEXT,
    last_seen_at    TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE TABLE routines (
    id               TEXT PRIMARY KEY,
    name             TEXT NOT NULL UNIQUE,
    mode             TEXT NOT NULL,
    prompt           TEXT NOT NULL,
    repositories     TEXT NOT NULL,            -- JSON array of names
    executor         TEXT NOT NULL DEFAULT 'claude-code',
    model            TEXT NOT NULL,            -- alias into [models]
    effort           TEXT,
    max_turns        INTEGER,
    timeout_seconds  INTEGER NOT NULL,
    max_budget_usd   REAL,
    allowed_tools    TEXT,                     -- JSON array; narrows the mode
    autonomy         TEXT,                     -- NULL = inherit
    verification     TEXT,                     -- NULL = mode level
    priority         INTEGER NOT NULL DEFAULT 50,
    budget_class     TEXT NOT NULL DEFAULT 'normal',
    schedule         TEXT,
    schedule_enabled INTEGER NOT NULL DEFAULT 0,
    concurrency      INTEGER NOT NULL DEFAULT 1,
    paths            TEXT,                     -- JSON globs (M9)
    deps             TEXT,                     -- JSON (M9)
    tier             INTEGER,
    models           TEXT,                     -- JSON aliases (M10)
    integrate        INTEGER NOT NULL DEFAULT 0,
    require_sandbox  INTEGER NOT NULL DEFAULT 1,
    max_questions    INTEGER NOT NULL DEFAULT 3,
    generation       INTEGER NOT NULL DEFAULT 1,
    next_due_at      TEXT,
    archived_at      TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);

CREATE TABLE routine_generations (
    routine_id          TEXT NOT NULL REFERENCES routines(id),
    generation          INTEGER NOT NULL,
    snapshot            TEXT NOT NULL,         -- JSON
    prompt_version_hash TEXT,
    source              TEXT NOT NULL,         -- edit | proposal:<id>
    created_at          TEXT NOT NULL,
    PRIMARY KEY (routine_id, generation)
);

CREATE TABLE work (
    id             TEXT PRIMARY KEY,
    routine_id     TEXT REFERENCES routines(id),
    routine_name   TEXT NOT NULL,
    generation     INTEGER NOT NULL,
    title          TEXT NOT NULL,
    trigger        TEXT NOT NULL,
    snapshot       TEXT NOT NULL,              -- JSON, frozen
    priority       INTEGER NOT NULL,
    budget_class   TEXT NOT NULL,
    autonomy       TEXT NOT NULL,
    integrate      INTEGER NOT NULL DEFAULT 0,
    paths          TEXT,
    deps           TEXT,
    tier           INTEGER,
    models         TEXT,
    plan_batch_id  TEXT,
    prompt_hash    TEXT,
    scheduled_for  TEXT,
    submitted_by   TEXT,
    external_refs  TEXT,                       -- JSON
    created_at     TEXT NOT NULL,
    finished_at    TEXT
);
CREATE INDEX work_created ON work(created_at);
CREATE INDEX work_routine ON work(routine_id, created_at);
CREATE INDEX work_open ON work(finished_at) WHERE finished_at IS NULL;

CREATE TABLE work_dependencies (
    work_id            TEXT NOT NULL REFERENCES work(id),
    blocked_by_work_id TEXT NOT NULL REFERENCES work(id),
    "on"               TEXT NOT NULL,          -- success | terminal
    stack_on           INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (work_id, blocked_by_work_id)
);

CREATE TABLE targets (
    id                TEXT PRIMARY KEY,
    work_id           TEXT NOT NULL REFERENCES work(id),
    repository_name   TEXT NOT NULL,
    state             TEXT NOT NULL,
    worker_id         TEXT,
    lease_token_hash  TEXT,
    lease_expires_at  TEXT,
    cancel_requested  INTEGER NOT NULL DEFAULT 0,
    retained          INTEGER NOT NULL DEFAULT 0,
    failure_reason    TEXT,
    unverified_reason TEXT,
    external_refs     TEXT,
    claimed_at        TEXT,
    started_at        TEXT,
    finished_at       TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    UNIQUE (work_id, repository_name)
);
CREATE INDEX targets_state ON targets(state);
CREATE INDEX targets_lease ON targets(lease_expires_at) WHERE lease_expires_at IS NOT NULL;

CREATE TABLE attempts (
    id                  TEXT PRIMARY KEY,
    target_id           TEXT NOT NULL REFERENCES targets(id),
    worker_id           TEXT NOT NULL,
    claim_request_id    TEXT NOT NULL UNIQUE,
    mcp_token_hash      TEXT NOT NULL,
    executor            TEXT NOT NULL,
    runner              TEXT,
    model               TEXT NOT NULL,         -- resolved id
    model_alias         TEXT NOT NULL,
    escalated_from      TEXT,
    routing             TEXT,                  -- JSON
    sandboxed           INTEGER NOT NULL DEFAULT 0,
    effort              TEXT,
    mode                TEXT NOT NULL,
    autonomy            TEXT NOT NULL,
    worktree_path       TEXT,
    branch              TEXT,
    base_branch         TEXT,
    base_commit         TEXT,
    stack_base_commit   TEXT,
    head_commit         TEXT,
    pid                 INTEGER,
    pid_start           INTEGER,
    session_id          TEXT,
    prompt_version_hash TEXT,
    launches            INTEGER NOT NULL DEFAULT 0,
    started_at          TEXT,
    finished_at         TEXT,
    exit_code           INTEGER,
    failure_reason      TEXT,
    unverified_reason   TEXT,
    is_error            INTEGER,
    result_text         TEXT,
    result              TEXT,                  -- JSON
    num_turns           INTEGER,
    input_tokens        INTEGER,
    output_tokens       INTEGER,
    cache_read_tokens   INTEGER,
    cache_creation_tokens INTEGER,
    cost_usd            REAL,
    git_dirty           INTEGER,
    git_commits         INTEGER,
    git_files_changed   INTEGER,
    git_insertions      INTEGER,
    git_deletions       INTEGER,
    git_pushed          INTEGER,
    verification_level  INTEGER,
    verification_passed INTEGER,
    cleanup_outcome     TEXT,
    cleanup_reason      TEXT,
    cleanup_command     TEXT,
    output_path         TEXT,
    output_bytes        INTEGER,
    output_truncated    INTEGER,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);
CREATE INDEX attempts_target ON attempts(target_id);

CREATE TABLE questions (
    id          TEXT PRIMARY KEY,
    attempt_id  TEXT NOT NULL REFERENCES attempts(id),
    target_id   TEXT NOT NULL REFERENCES targets(id),
    work_id     TEXT NOT NULL REFERENCES work(id),
    text        TEXT NOT NULL,
    options     TEXT,                          -- JSON
    context     TEXT,                          -- JSON
    checkpoint  TEXT,
    answer      TEXT,
    answered_by TEXT,
    asked_at    TEXT NOT NULL,
    answered_at TEXT
);
CREATE INDEX questions_open ON questions(answered_at) WHERE answered_at IS NULL;

CREATE TABLE events (
    attempt_id  TEXT NOT NULL REFERENCES attempts(id),
    source      TEXT NOT NULL,                 -- worker | mcp | control
    seq         INTEGER NOT NULL,
    time        TEXT NOT NULL,
    elapsed_us  INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    message     TEXT NOT NULL,
    span_id     TEXT,
    parent_id   TEXT,
    name        TEXT,
    duration_us INTEGER,
    attrs       TEXT,
    PRIMARY KEY (attempt_id, source, seq)
) WITHOUT ROWID;

CREATE TABLE attempt_facts (
    attempt_id            TEXT PRIMARY KEY REFERENCES attempts(id),
    target_id             TEXT NOT NULL,
    work_id               TEXT NOT NULL,
    routine               TEXT NOT NULL,
    generation            INTEGER NOT NULL,
    project               TEXT NOT NULL,
    repository            TEXT NOT NULL,
    worker                TEXT NOT NULL,
    executor              TEXT NOT NULL,
    model                 TEXT NOT NULL,
    effort                TEXT,
    mode                  TEXT NOT NULL,
    trigger               TEXT NOT NULL,
    prompt_version_hash   TEXT,
    autonomy              TEXT NOT NULL,
    queue_wait_us         INTEGER,
    fetch_us              INTEGER,
    resolve_base_us       INTEGER,
    worktree_add_us       INTEGER,
    manifest_us           INTEGER,
    agent_us              INTEGER,
    git_inspect_us        INTEGER,
    verify_us             INTEGER,
    cleanup_us            INTEGER,
    total_us              INTEGER,
    started_at            TEXT,
    finished_at           TEXT NOT NULL,
    turns                 INTEGER,
    input_tokens          INTEGER,
    output_tokens         INTEGER,
    cache_read_tokens     INTEGER,
    cache_creation_tokens INTEGER,
    cost_usd              REAL,
    tool_calls_total      INTEGER,
    tool_calls_by_name    TEXT,
    tool_time_us_by_name  TEXT,
    tool_p50_us           INTEGER,
    tool_max_us           INTEGER,
    tool_errors           INTEGER,
    questions_asked       INTEGER,
    wait_human_us         INTEGER,
    events_total          INTEGER,
    events_dropped        INTEGER,
    state                 TEXT NOT NULL,
    exit_code             INTEGER,
    failure_reason        TEXT,
    is_error              INTEGER,
    verification_level    INTEGER,
    verification_passed   INTEGER,
    retained              INTEGER,
    retained_reason       TEXT,
    commits               INTEGER,
    files_changed         INTEGER,
    insertions            INTEGER,
    deletions             INTEGER,
    dirty                 INTEGER,
    pushed                INTEGER,
    branch                TEXT,
    base                  TEXT,
    head                  TEXT,
    five_hour_before      REAL,
    five_hour_after       REAL,
    seven_day_before      REAL,
    seven_day_after       REAL,
    utilization_delta_estimate REAL,
    declared_paths        TEXT,
    touched_paths         TEXT,
    write_set_precision   REAL,
    lease_wait_us         INTEGER,
    merge_wait_us         INTEGER,
    rebase_attempts       INTEGER,
    merge_outcome         TEXT,
    stack_depth           INTEGER,
    usd                   REAL,
    five_hour_delta       REAL,
    seven_day_delta       REAL,
    runner_seconds        REAL,
    runner                TEXT,
    model_alias           TEXT,
    model_class           TEXT,
    escalated_from        TEXT,
    tokens_to_first_edit  INTEGER,
    questions_changed_outcome INTEGER
);
CREATE INDEX facts_routine_finished ON attempt_facts(routine, finished_at);
CREATE INDEX facts_repository_finished ON attempt_facts(repository, finished_at);
CREATE INDEX facts_routine_generation ON attempt_facts(routine, generation);
CREATE INDEX facts_prompt ON attempt_facts(prompt_version_hash);

CREATE TABLE rate_limit_samples (
    ts             TEXT NOT NULL,
    window         TEXT NOT NULL,              -- five_hour | seven_day
    utilization    REAL NOT NULL,
    resets_at      TEXT NOT NULL,
    source_attempt TEXT NOT NULL DEFAULT '',   -- '' rather than NULL so the key dedupes
    PRIMARY KEY (ts, window, source_attempt)
);
CREATE INDEX samples_window_ts ON rate_limit_samples(window, ts);

CREATE TABLE prompt_versions (
    hash             TEXT PRIMARY KEY,
    routine          TEXT NOT NULL,
    generation       INTEGER NOT NULL,
    mode             TEXT NOT NULL,
    template         TEXT NOT NULL,
    rendered_example TEXT NOT NULL,
    system_append    TEXT NOT NULL,
    tool_list        TEXT NOT NULL,            -- JSON
    model            TEXT NOT NULL,
    effort           TEXT,
    created_at       TEXT NOT NULL
);

CREATE TABLE proposals (
    id                TEXT PRIMARY KEY,
    source            TEXT NOT NULL,
    kind              TEXT NOT NULL,
    target            TEXT NOT NULL,
    before            TEXT,                    -- JSON
    after             TEXT,                    -- JSON
    rationale         TEXT NOT NULL,
    verification_plan TEXT NOT NULL,
    status            TEXT NOT NULL,
    decided_by        TEXT,
    decided_at        TEXT,
    applied_ref       TEXT,
    outcome_metrics   TEXT,                    -- JSON
    eval_score        REAL,
    external_refs     TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

CREATE TABLE kb_notes (
    id        TEXT PRIMARY KEY,
    path      TEXT NOT NULL UNIQUE,
    title     TEXT NOT NULL,
    type      TEXT NOT NULL,
    created   TEXT NOT NULL,
    tags      TEXT NOT NULL,                   -- JSON
    mtime     INTEGER NOT NULL,
    hash      TEXT NOT NULL,
    body_hash TEXT NOT NULL,
    indexed_at TEXT NOT NULL
);
CREATE TABLE kb_links (
    from_id   TEXT NOT NULL REFERENCES kb_notes(id) ON DELETE CASCADE,
    to_kind   TEXT NOT NULL,                   -- note | attempt | work | target | routine | repository | project | proposal | prompt
    to_ref    TEXT NOT NULL,
    link_type TEXT NOT NULL,                   -- inline | about | supersedes | evidence_for
    PRIMARY KEY (from_id, to_kind, to_ref, link_type)
);
CREATE INDEX kb_links_to ON kb_links(to_kind, to_ref);
CREATE VIRTUAL TABLE kb_fts USING fts5(id UNINDEXED, title, body);

CREATE TABLE workers (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    version        TEXT NOT NULL,
    max_concurrent INTEGER NOT NULL,
    active         INTEGER NOT NULL DEFAULT 0,
    executors      TEXT NOT NULL,              -- JSON
    capabilities   TEXT NOT NULL,              -- JSON
    registered_at  TEXT NOT NULL,
    last_seen_at   TEXT NOT NULL
);
CREATE TABLE retained_worktrees (
    attempt_id      TEXT PRIMARY KEY,
    worker_id       TEXT NOT NULL,
    path            TEXT NOT NULL,
    reason          TEXT NOT NULL,
    cleanup_command TEXT NOT NULL,
    reported_at     TEXT NOT NULL
);

CREATE TABLE plugins (
    name         TEXT PRIMARY KEY,
    version      TEXT NOT NULL,
    kind         TEXT NOT NULL,                -- first_party | third_party
    path         TEXT NOT NULL,
    enabled      INTEGER NOT NULL DEFAULT 0,
    scopes       TEXT NOT NULL,                -- JSON
    token_hash   TEXT,
    cursor       INTEGER,
    installed_at TEXT NOT NULL,
    enabled_at   TEXT
);

CREATE TABLE verifications (
    id                  TEXT PRIMARY KEY,
    attempt_id          TEXT NOT NULL REFERENCES attempts(id),
    level               INTEGER NOT NULL,
    passed              INTEGER NOT NULL,
    verifier_attempt_id TEXT,
    verdict             TEXT,                  -- JSON
    decided_by          TEXT,
    created_at          TEXT NOT NULL
);
CREATE INDEX verifications_attempt ON verifications(attempt_id);

CREATE TABLE artifacts (
    id         TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    kind       TEXT NOT NULL,
    path       TEXT NOT NULL,
    bytes      INTEGER NOT NULL,
    sha256     TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE path_leases (
    target_id       TEXT PRIMARY KEY REFERENCES targets(id),
    repository_name TEXT NOT NULL,
    globs           TEXT NOT NULL,             -- JSON
    acquired_at     TEXT NOT NULL
);
CREATE INDEX path_leases_repo ON path_leases(repository_name);

CREATE TABLE merges (
    id                 TEXT PRIMARY KEY,
    target_id          TEXT NOT NULL REFERENCES targets(id),
    repository_name    TEXT NOT NULL,
    integration_branch TEXT NOT NULL,
    before_sha         TEXT,
    after_sha          TEXT,
    rebase_attempts    INTEGER NOT NULL DEFAULT 0,
    outcome            TEXT,
    pushed_at          TEXT,
    created_at         TEXT NOT NULL
);

CREATE TABLE runners (
    name          TEXT PRIMARY KEY,
    kind          TEXT NOT NULL,
    billing       TEXT NOT NULL,
    capacity      INTEGER NOT NULL,
    endpoint      TEXT,
    health        TEXT,
    last_probe_at TEXT
);

CREATE TABLE evals (
    id                  TEXT PRIMARY KEY,
    mode                TEXT NOT NULL,
    prompt_version_hash TEXT NOT NULL,
    model_alias         TEXT NOT NULL,
    fixture             TEXT NOT NULL,
    verified            INTEGER NOT NULL,
    usd                 REAL,
    turns               INTEGER,
    created_at          TEXT NOT NULL
);

CREATE TABLE backups (
    id         TEXT PRIMARY KEY,
    path       TEXT NOT NULL,
    bytes      INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

-- The audit trail. id is the monotonic order of events in the system.
CREATE TABLE journal (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts          TEXT NOT NULL,
    kind        TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id   TEXT NOT NULL,
    payload     TEXT NOT NULL                  -- JSON
);
CREATE INDEX journal_entity ON journal(entity_type, entity_id);
