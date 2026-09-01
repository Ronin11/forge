-- Repository archival: an archived repository is removed from the local system
-- (its checkout deleted, dropped from worker.toml) but keeps its row and every
-- fact, attempt, and kb note that references it by name — the metadata survives.
-- origin_url is the raw clone URL captured at archive time so Restore can
-- re-clone it. Defaults keep existing rows working, and neither column is in the
-- worker Register upsert's ON CONFLICT SET list, so a re-register preserves them.
ALTER TABLE repositories ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;
ALTER TABLE repositories ADD COLUMN archived_at TEXT NOT NULL DEFAULT '';
ALTER TABLE repositories ADD COLUMN origin_url TEXT NOT NULL DEFAULT '';
