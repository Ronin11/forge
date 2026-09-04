-- Routines are triggers only: the content columns are gone (content lives in
-- the directives library; pre-restructure content survives in
-- routine_generations snapshots and the library's git history). target
-- becomes required.
ALTER TABLE routines DROP COLUMN mode;
ALTER TABLE routines DROP COLUMN prompt;
ALTER TABLE routines DROP COLUMN persona;
ALTER TABLE routines DROP COLUMN model;
ALTER TABLE routines DROP COLUMN effort;
