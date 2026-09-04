-- Prompt test runs: the Prompts page's iterate loop. Each run of
-- POST /api/v1/prompt-test is recorded against its subject (persona:<name>
-- or routine:<name>) with the inputs, the chosen model, the composed prompt
-- and manifest (so a later reader knows which fragment versions produced the
-- output), and the model's output. The store keeps the most recent handful
-- per subject — this is a scratchpad for prompt iteration, not an archive.

CREATE TABLE prompt_tests (
    id          TEXT PRIMARY KEY,
    subject     TEXT NOT NULL,          -- persona:<name> | routine:<name>
    persona     TEXT,
    routine     TEXT,
    mode        TEXT,
    task        TEXT,
    objective   TEXT,
    repo        TEXT,
    model       TEXT NOT NULL,
    prompt      TEXT,                   -- the composed prompt, size-capped
    composition TEXT,                   -- JSON prompts.Composition
    output      TEXT,                   -- the model's reply, size-capped
    elapsed_ms  INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL
);

CREATE INDEX idx_prompt_tests_subject ON prompt_tests (subject, created_at);
