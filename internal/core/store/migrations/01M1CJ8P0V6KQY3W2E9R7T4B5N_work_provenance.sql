-- Work provenance (DESIGN.md §3 "Provenance"): every Work carries where it came
-- from and which root intent it serves, so the task tree is walkable backward
-- ("why does this exist?") and forward ("what did it spawn?").
--
--   caused_by_work_id  the Work whose execution created this one; NULL for roots.
--   root_work_id       the root of this Work's tree — own id for a root, else the
--                      parent's root. Denormalized on purpose: "give me the whole
--                      tree" is one indexed query, not a recursive walk. NOT NULL
--                      for rows created after this migration (CreateWork enforces
--                      it); nullable only so the ALTER can precede the backfill.
--   cause              short machine label for the functional reason
--                      (plan_task | verify | follow_up); empty for roots.
--
-- plan_batch_id and the snapshot's verify_of stay as-is (readers exist); these
-- columns are the walkable superset, redundant on purpose.
ALTER TABLE work ADD COLUMN caused_by_work_id TEXT REFERENCES work(id);
ALTER TABLE work ADD COLUMN root_work_id TEXT;
ALTER TABLE work ADD COLUMN cause TEXT;
CREATE INDEX idx_work_root ON work (root_work_id);
CREATE INDEX idx_work_caused_by ON work (caused_by_work_id) WHERE caused_by_work_id IS NOT NULL;

-- Backfill, in this migration so the columns are never observed half-populated.
-- Plan children: the plan Work is the immediate cause.
UPDATE work SET caused_by_work_id = plan_batch_id, cause = 'plan_task'
 WHERE plan_batch_id IS NOT NULL;

-- Verify follow-ups: the subject lives in the snapshot at attempt granularity
-- (verify_of.attempt_id → attempts.id → targets.work_id). json_extract needs the
-- json1 extension; if this SQLite build lacks it the statement errors and the
-- migration fails loudly here, rather than silently skipping the link.
UPDATE work SET
    caused_by_work_id = (
        SELECT t.work_id FROM attempts a JOIN targets t ON t.id = a.target_id
         WHERE a.id = json_extract(work.snapshot, '$.verify_of.attempt_id')),
    cause = 'verify'
 WHERE caused_by_work_id IS NULL
   AND json_extract(snapshot, '$.verify_of.attempt_id') IS NOT NULL;

-- Roots: follow caused_by_work_id to its end for every row (recursive CTE, not
-- depth-limited) and denormalize the terminal id into root_work_id.
WITH RECURSIVE roots(id, root) AS (
    SELECT id, id FROM work WHERE caused_by_work_id IS NULL
    UNION ALL
    SELECT w.id, r.root FROM work w JOIN roots r ON w.caused_by_work_id = r.id
)
UPDATE work SET root_work_id = (SELECT root FROM roots WHERE roots.id = work.id);
