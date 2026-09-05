-- Scratch scripts: the organic layer under the curated library. Agents save
-- and run quick scripts through forge_scratch; rows are an LRU cache
-- (bounded, evicted by last use), and one that keeps getting reached for is
-- promoted automatically into the git library's scripts/ tree.
CREATE TABLE scratch_scripts (
    name         TEXT PRIMARY KEY,
    source       TEXT NOT NULL,
    hash         TEXT NOT NULL,
    language     TEXT NOT NULL,             -- extension without the dot: js, py, sh, ...
    description  TEXT NOT NULL,
    input_schema TEXT NOT NULL DEFAULT '',
    created_by   TEXT NOT NULL,             -- agent:<attempt> | human
    run_count    INTEGER NOT NULL DEFAULT 0,
    attempts     TEXT NOT NULL DEFAULT '[]',-- distinct caller attempts (bounded)
    created_at   TEXT NOT NULL,
    last_run_at  TEXT NOT NULL
);
CREATE INDEX idx_scratch_last_run ON scratch_scripts(last_run_at);
