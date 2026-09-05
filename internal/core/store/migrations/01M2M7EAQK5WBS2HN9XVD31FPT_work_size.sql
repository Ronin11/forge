-- Work gains an optional size bucket (S|M|L, '' = unsized): assigned by the
-- submitter or the planning agent, frozen at creation, copied into
-- attempt_facts so size-vs-actual-cost calibration is queryable.
ALTER TABLE work ADD COLUMN size TEXT NOT NULL DEFAULT '';
