-- The legacy steps form is gone: WorkflowGraph is the only definition and the
-- insert no longer supplies steps, which was NOT NULL — so the column must go
-- or every workflow create fails. Legacy rows already carry a backfilled
-- graph (the previous release's open-time backfill), and old generation
-- snapshots keep their steps JSON inside the snapshot text.
ALTER TABLE workflows DROP COLUMN steps;
