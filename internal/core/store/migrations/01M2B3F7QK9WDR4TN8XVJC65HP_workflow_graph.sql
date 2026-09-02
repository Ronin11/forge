-- Graph workflows: the canonical definition becomes a typed node/edge graph
-- (workflow_graph.go) instead of a linear steps list. The column is added
-- NULLable and backfilled in Go at open (backfillWorkflowGraphs) so the
-- lossless steps→graph conversion lives in exactly one place; `steps` stays
-- for old generation snapshots and API input compatibility.

ALTER TABLE workflows ADD COLUMN graph TEXT;
