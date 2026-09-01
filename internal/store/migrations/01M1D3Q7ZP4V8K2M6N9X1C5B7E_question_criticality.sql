-- Fuzzy Human Queue (DESIGN.md §10.4): a Question carries a criticality so the
-- attention sweep can auto-decide non-critical ones once their time-of-day SLA
-- lapses, and a rationale recording why an auto-decision was made (rendered on
-- the task page and the Human Queue so the auto path is unmistakable).
--
--   criticality  critical | normal | low; agent-declared, default normal. Only
--                critical always blocks for a human. Existing rows backfill to
--                normal via the column default.
--   rationale    the decider model's reason for an auto-answer; NULL for a
--                human answer or an open question.
ALTER TABLE questions ADD COLUMN criticality TEXT NOT NULL DEFAULT 'normal';
ALTER TABLE questions ADD COLUMN rationale TEXT;
