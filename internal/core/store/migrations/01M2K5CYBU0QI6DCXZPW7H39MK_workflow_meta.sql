-- Workflow metadata for search and the agent tool bridge: a one-line
-- description and a tool flag (agents may fire tool-flagged workflows via
-- forge_workflow_run). Library content carries its metadata in-file; workflow
-- rows are the SQLite exception and ship via seeds JSON.
ALTER TABLE workflows ADD COLUMN description TEXT NOT NULL DEFAULT '';
ALTER TABLE workflows ADD COLUMN tool INTEGER NOT NULL DEFAULT 0;
