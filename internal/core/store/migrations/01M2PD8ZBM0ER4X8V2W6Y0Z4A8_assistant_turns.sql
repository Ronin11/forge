-- Sessionized assistant chat (the Signal front door): turns persist across
-- daemon restarts, carry the entity each exchange touched (a filed task, an
-- answered question), and session boundaries are derived from idle gaps at
-- read time — no session table to maintain.
CREATE TABLE assistant_turns (
    id             TEXT PRIMARY KEY,
    sender         TEXT NOT NULL,
    user_text      TEXT NOT NULL,
    assistant_text TEXT NOT NULL,
    action         TEXT NOT NULL DEFAULT '',
    ref            TEXT NOT NULL DEFAULT '',  -- "work:<id>" | "question:<id>"
    created_at     TEXT NOT NULL
);
CREATE INDEX idx_assistant_turns_sender ON assistant_turns(sender, created_at DESC);
