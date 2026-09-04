-- Routines become trigger shells: target names what a run invokes
-- ("directive:<name>" | "workflow:<name>"), objective is the default
-- {{objective}} for runs the trigger creates. Content columns (mode, prompt,
-- persona, model, effort) stay for legacy rows and generation history; a
-- target row leaves them empty.
ALTER TABLE routines ADD COLUMN target TEXT;
ALTER TABLE routines ADD COLUMN objective TEXT;
