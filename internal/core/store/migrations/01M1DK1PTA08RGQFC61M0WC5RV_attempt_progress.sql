-- Live per-attempt progress (DESIGN.md §8): a running attempt's turn/token
-- tally, latest heartbeat phase, and most recent forge_note_progress note, kept
-- current as events and heartbeats arrive so a healthy-but-early agent is
-- distinguishable from a wedged one. The authoritative totals still land on the
-- attempts row at completion; this side table is the in-flight view and is never
-- read once the attempt is finished.
--
--   running_turns      assistant turns so far — one per de-duplicated "usage"
--                      metric the parser emits.
--   input_tokens       running input/output token sums across those usage
--   output_tokens      metrics; approximate, superseded by the attempt's
--                      authoritative Usage at completion.
--   last_event_at      time of the most recent ingested event.
--   hb_state           latest heartbeat State (preparing | running) and Phase,
--   phase              recorded at last_heartbeat_at.
--   last_heartbeat_at
--   progress_note      latest forge_note_progress message and its declared
--   checkpoint         checkpoint, recorded at progress_at.
--   progress_at
--
-- Turns/tokens/last_event/note are recomputed from the events table (idempotent
-- under at-least-once event delivery); the heartbeat columns are upserted from
-- the heartbeat handler.
CREATE TABLE attempt_progress (
    attempt_id        TEXT PRIMARY KEY REFERENCES attempts(id),
    running_turns     INTEGER NOT NULL DEFAULT 0,
    input_tokens      INTEGER NOT NULL DEFAULT 0,
    output_tokens     INTEGER NOT NULL DEFAULT 0,
    last_event_at     TEXT,
    hb_state          TEXT,
    phase             TEXT,
    last_heartbeat_at TEXT,
    progress_note     TEXT,
    checkpoint        TEXT,
    progress_at       TEXT,
    updated_at        TEXT NOT NULL
);
