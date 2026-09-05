-- Live multivariant experiments: N arms of a directive/persona assigned to
-- real production work at materialization, per-arm bookkeeping in
-- attempt_facts, decision on the sweep tick, promotion guarded by the A/B
-- revert net.
ALTER TABLE experiments ADD COLUMN kind TEXT NOT NULL DEFAULT 'offline'; -- offline | live
ALTER TABLE experiments ADD COLUMN arms TEXT;        -- JSON [{label,title,content,hash}], arms[0] = control
ALTER TABLE experiments ADD COLUMN min_runs INTEGER NOT NULL DEFAULT 0;
ALTER TABLE experiments ADD COLUMN opened_at TEXT;   -- when status became live
ALTER TABLE experiments ADD COLUMN decide_by TEXT;   -- past due with short arms => inconclusive
-- One live experiment per subject, across setup and assignment.
CREATE UNIQUE INDEX idx_experiments_live_subject ON experiments(subject)
    WHERE kind = 'live' AND status IN ('running','live');

-- Per-arm facts attribution; persona makes promoted persona edits
-- A/B-guardable the way directive edits already are.
ALTER TABLE attempt_facts ADD COLUMN experiment_id TEXT NOT NULL DEFAULT '';
ALTER TABLE attempt_facts ADD COLUMN variant TEXT NOT NULL DEFAULT '';  -- 'control' | 'v1'…
ALTER TABLE attempt_facts ADD COLUMN persona TEXT NOT NULL DEFAULT '';
CREATE INDEX idx_facts_experiment ON attempt_facts(experiment_id, finished_at) WHERE experiment_id != '';
CREATE INDEX idx_facts_persona ON attempt_facts(persona, finished_at) WHERE persona != '';
