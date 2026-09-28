//! Sessions and their turns as the JSON `forge chat sessions` and `forge
//! chat show` print, and the web client's Chat page reads.

use super::record;
use crate::ctx::Forge;
use anyhow::{Context, Result};
use serde_json::{Value, json};

fn session_row(s: &crate::store::ChatSession) -> Value {
    json!({
        "id": s.id, "title": s.title, "provider": s.provider,
        "created_at": s.created_at, "updated_at": s.updated_at,
        "turns": s.turns, "cost_usd": s.cost_usd,
    })
}

/// The most recent sessions, newest first.
pub fn sessions_doc(f: &Forge, limit: u32) -> Result<Value> {
    let rows: Vec<Value> = f
        .store
        .chat_sessions(limit)?
        .iter()
        .map(session_row)
        .collect();
    Ok(json!({ "sessions": rows }))
}

/// One session with every turn. A turn's tool calls carry, for a write
/// verb's proposal, the `action` id to confirm or reject it by.
pub fn session_doc(f: &Forge, id: i64) -> Result<Value> {
    let s = f
        .store
        .chat_session(id)?
        .with_context(|| format!("no chat session {id}"))?;
    let turns: Vec<Value> = f
        .store
        .chat_turns(id)?
        .iter()
        .map(|t| {
            let calls: Vec<Value> = record::parse_calls(&t.tool_calls)
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let mut v = serde_json::to_value(c).unwrap_or_default();
                    if c.proposal.is_some() {
                        v["action"] = record::action_id(t.id, i).into();
                    }
                    v
                })
                .collect();
            json!({
                "id": t.id, "role": t.role, "text": t.text, "tool_calls": calls,
                "cost_usd": t.cost_usd, "provider": t.provider, "model": t.model,
                "prompt_hash": t.prompt_hash, "at": t.at,
            })
        })
        .collect();
    Ok(json!({ "session": session_row(&s), "turns": turns }))
}
