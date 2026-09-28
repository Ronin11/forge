//! The record of Ask Forge (docs/CHAT.md): a session is a conversation, a
//! turn is one thing said in it — the operator's message, the model's
//! reply with every tool call it made and what each returned, or the
//! outcome of an action the operator confirmed. The cost of every model
//! call is on its turn and counts against the operator's per-day budget
//! (`Store::spent_since`).

use super::*;

/// One conversation with Forge.
#[derive(Debug, Clone)]
pub struct ChatSession {
    pub id: i64,
    /// The first thing the operator said, cut short.
    pub title: String,
    /// The provider the session's first model call ran under.
    pub provider: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub turns: i64,
    pub cost_usd: f64,
}

/// One turn of a session. `role` is `user`, `assistant`, or `action` (the
/// outcome of a proposed action the operator confirmed or rejected).
#[derive(Debug, Clone)]
pub struct ChatTurn {
    pub id: i64,
    pub session: i64,
    pub role: String,
    pub text: String,
    /// A JSON array: each tool call with its arguments, its result, and
    /// for a write verb the state of the proposal.
    pub tool_calls: String,
    pub cost_usd: f64,
    pub provider: String,
    pub model: String,
    /// The hash of the `chat` directive's text this turn ran under.
    pub prompt_hash: String,
    pub at: i64,
}

/// What `Store::insert_chat_turn` records.
pub struct NewChatTurn<'a> {
    pub session: i64,
    pub role: &'a str,
    pub text: &'a str,
    pub tool_calls: &'a str,
    pub cost_usd: f64,
    pub provider: &'a str,
    pub model: &'a str,
    pub prompt_hash: &'a str,
}

#[cfg(test)]
pub(super) const CHAT_SESSION_COLUMNS: &[&str] =
    &["id", "title", "provider", "created_at", "updated_at"];

pub(super) const CHAT_TURN_COLUMNS: &[&str] = &[
    "id",
    "session",
    "role",
    "text",
    "tool_calls",
    "cost_usd",
    "provider",
    "model",
    "prompt_hash",
    "at",
];

fn chat_turn_from_row(r: &Row) -> rusqlite::Result<ChatTurn> {
    Ok(ChatTurn {
        id: r.get("id")?,
        session: r.get("session")?,
        role: r.get("role")?,
        text: r.get("text")?,
        tool_calls: r.get("tool_calls")?,
        cost_usd: r.get("cost_usd")?,
        provider: r.get("provider")?,
        model: r.get("model")?,
        prompt_hash: r.get("prompt_hash")?,
        at: r.get("at")?,
    })
}

fn chat_session_from_row(r: &Row) -> rusqlite::Result<ChatSession> {
    Ok(ChatSession {
        id: r.get("id")?,
        title: r.get("title")?,
        provider: r.get("provider")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        turns: r.get("turns")?,
        cost_usd: r.get("cost_usd")?,
    })
}

const SESSION_SELECT: &str = "SELECT s.id AS id, s.title AS title, s.provider AS provider,
        s.created_at AS created_at, s.updated_at AS updated_at,
        (SELECT COUNT(*) FROM chat_turns t WHERE t.session = s.id) AS turns,
        (SELECT COALESCE(SUM(t.cost_usd), 0) FROM chat_turns t WHERE t.session = s.id) AS cost_usd
     FROM chat_sessions s";

impl Store {
    pub fn create_chat_session(&self, title: &str, provider: &str) -> Result<i64> {
        let now = crate::unix_now();
        let c = self.lock();
        c.retry_execute(
            "INSERT INTO chat_sessions (title, provider, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)",
            params![title, provider, now],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn chat_session(&self, id: i64) -> Result<Option<ChatSession>> {
        Ok(self
            .lock()
            .retry_query_row(
                &format!("{SESSION_SELECT} WHERE s.id = ?1"),
                params![id],
                chat_session_from_row,
            )
            .optional()?)
    }

    /// The most recently active sessions, newest first.
    pub fn chat_sessions(&self, limit: u32) -> Result<Vec<ChatSession>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "{SESSION_SELECT} ORDER BY s.updated_at DESC, s.id DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map(params![limit], chat_session_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn insert_chat_turn(&self, t: &NewChatTurn<'_>) -> Result<i64> {
        let now = crate::unix_now();
        let c = self.lock();
        c.retry_execute(
            "INSERT INTO chat_turns
               (session, role, text, tool_calls, cost_usd, provider, model, prompt_hash, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                t.session,
                t.role,
                t.text,
                t.tool_calls,
                t.cost_usd,
                t.provider,
                t.model,
                t.prompt_hash,
                now
            ],
        )?;
        let id = c.last_insert_rowid();
        c.retry_execute(
            "UPDATE chat_sessions SET updated_at = ?2 WHERE id = ?1",
            params![t.session, now],
        )?;
        Ok(id)
    }

    /// A session's turns, oldest first.
    pub fn chat_turns(&self, session: i64) -> Result<Vec<ChatTurn>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM chat_turns WHERE session = ?1 ORDER BY id",
            CHAT_TURN_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![session], chat_turn_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn chat_turn(&self, id: i64) -> Result<Option<ChatTurn>> {
        Ok(self
            .lock()
            .retry_query_row(
                &format!(
                    "SELECT {} FROM chat_turns WHERE id = ?1",
                    CHAT_TURN_COLUMNS.join(", ")
                ),
                params![id],
                chat_turn_from_row,
            )
            .optional()?)
    }

    /// Replace a turn's `tool_calls` only if it still reads `expected`:
    /// the compare-and-swap that lets exactly one of two racing
    /// confirmations of the same proposed action run it. Whether this one
    /// won.
    pub fn swap_chat_tool_calls(&self, id: i64, expected: &str, new: &str) -> Result<bool> {
        let n = self.lock().retry_execute(
            "UPDATE chat_turns SET tool_calls = ?3 WHERE id = ?1 AND tool_calls = ?2",
            params![id, expected, new],
        )?;
        Ok(n == 1)
    }
}
