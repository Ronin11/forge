-- Personas: routines may name a persona from the file-backed prompts library
-- (~/.forge/prompts, git-versioned Markdown). The persona's resolved text is
-- composed into the frozen snapshot at Work creation; `composition` records
-- the audit manifest — persona, mode, the prompts repo's commit (and whether
-- the tree was dirty), and the content hash of every fragment that went in —
-- so "what did this run read, and which fragment edit changed the outcome"
-- are queries, not archaeology.

ALTER TABLE routines ADD COLUMN persona TEXT;
ALTER TABLE work ADD COLUMN persona TEXT;
ALTER TABLE work ADD COLUMN composition TEXT;  -- JSON prompts.Composition
