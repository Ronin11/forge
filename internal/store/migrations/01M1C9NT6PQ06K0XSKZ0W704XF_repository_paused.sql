-- Repository controls (M12+): a per-repository pause flag the scheduler honours
-- (a paused repository's targets are not admitted, like a budget stop), and an
-- optional running-app URL an operator can open from the repository page. Both
-- default to their empty value so an existing row keeps working, and the worker's
-- Register upsert preserves them (neither is in its ON CONFLICT SET list).
ALTER TABLE repositories ADD COLUMN paused INTEGER NOT NULL DEFAULT 0;
ALTER TABLE repositories ADD COLUMN app_url TEXT NOT NULL DEFAULT '';
