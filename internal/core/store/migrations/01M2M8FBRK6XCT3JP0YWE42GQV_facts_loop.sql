-- The learning-loop dimensions: facts gain the work tree root (rollups per
-- ask), the size bucket (calibration), the workflow name, and the 1-5 quality
-- scores a judging attempt emitted in its result `scores` object. Score
-- columns are nullable — absent is not zero.
ALTER TABLE attempt_facts ADD COLUMN root_work_id TEXT;
ALTER TABLE attempt_facts ADD COLUMN size TEXT NOT NULL DEFAULT '';
ALTER TABLE attempt_facts ADD COLUMN workflow_name TEXT NOT NULL DEFAULT '';
ALTER TABLE attempt_facts ADD COLUMN score_overall INTEGER;
ALTER TABLE attempt_facts ADD COLUMN score_correctness INTEGER;
ALTER TABLE attempt_facts ADD COLUMN score_completeness INTEGER;
ALTER TABLE attempt_facts ADD COLUMN score_quality INTEGER;
ALTER TABLE attempt_facts ADD COLUMN score_effort_fit INTEGER;
CREATE INDEX idx_facts_root ON attempt_facts(root_work_id) WHERE root_work_id IS NOT NULL;
