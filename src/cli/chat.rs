//! `forge chat`: Ask Forge (docs/CHAT.md). A message continues a session
//! or opens one; `sessions`, `show`, `confirm` and `reject` are the rest
//! of the conversation. The model's work is in `crate::chat`; this prints.

use super::*;
use crate::chat;
use serde_json::{Value, json};

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct ChatCmd {
    #[command(subcommand)]
    cmd: Option<ChatSub>,
    /// Continue this session (default: open a new one)
    #[arg(long)]
    session: Option<i64>,
    /// Ask under this provider (default: the first whose runner is chat,
    /// else the operator's default)
    #[arg(long)]
    provider: Option<String>,
    /// Print the reply and its tool calls as one JSON object
    #[arg(long, conflicts_with = "stream")]
    json: bool,
    /// Print each event as a JSON line as it happens: the session, every
    /// tool call, and the reply
    #[arg(long)]
    stream: bool,
    /// What to ask Forge, or tell it to do
    message: Vec<String>,
}

#[derive(Subcommand)]
enum ChatSub {
    /// List sessions, most recently active first
    Sessions {
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Show a session: every turn, its tool calls, its cost
    Show {
        id: i64,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Confirm a proposed action (the id `forge chat` printed, like 12.0):
    /// runs it once and records it as a decision
    Confirm {
        action: String,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Reject a proposed action: nothing runs
    Reject {
        action: String,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
}

fn print_proposals(proposals: &[Value]) {
    for p in proposals {
        let (action, summary) = (
            p["action"].as_str().unwrap_or(""),
            p["summary"].as_str().unwrap_or(""),
        );
        out!("proposed {action}: {summary}");
        out!("  forge chat confirm {action}    forge chat reject {action}");
    }
}

async fn say(c: ChatCmd) -> Result<()> {
    let message = c.message.join(" ");
    if message.trim().is_empty() {
        bail!("say something: forge chat [--session <id>] <message>");
    }
    let f = Forge::open(false, false)?;
    let stream = c.stream;
    let mut emit = |event: Value| {
        if stream {
            out!("{event}");
        }
    };
    let asked = chat::ask(&f, c.session, &message, c.provider.as_deref(), &mut emit).await?;
    let proposals = asked.proposals();
    if c.json {
        let doc = chat::session_doc(&f, asked.session)?;
        let calls = doc["turns"]
            .as_array()
            .and_then(|t| t.iter().find(|t| t["id"] == asked.turn))
            .map(|t| t["tool_calls"].clone())
            .unwrap_or(json!([]));
        out!(
            "{}",
            json!({
                "session": asked.session, "turn": asked.turn, "reply": asked.reply,
                "cost_usd": asked.cost_usd, "tool_calls": calls, "proposals": proposals,
            })
        );
    } else if !c.stream {
        out!("{}", asked.reply);
        print_proposals(&proposals);
        out!("(session {}, ${:.4})", asked.session, asked.cost_usd);
    }
    Ok(())
}

fn sessions(limit: u32, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let doc = chat::sessions_doc(&f, limit)?;
    if json {
        out!("{doc}");
        return Ok(());
    }
    let rows = doc["sessions"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        out!("no chat sessions");
    }
    for s in &rows {
        out!(
            "{:<5} {}  {} turn(s)  ${:.4}  {}",
            s["id"],
            crate::render::utc(s["updated_at"].as_i64().unwrap_or(0)),
            s["turns"],
            s["cost_usd"].as_f64().unwrap_or(0.0),
            s["title"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

fn show(id: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let doc = chat::session_doc(&f, id)?;
    if json {
        out!("{doc}");
        return Ok(());
    }
    out!(
        "session {id}: {}",
        doc["session"]["title"].as_str().unwrap_or("")
    );
    for t in doc["turns"].as_array().into_iter().flatten() {
        let who = match t["role"].as_str().unwrap_or("") {
            "user" => "you",
            "assistant" => "forge",
            _ => "action",
        };
        out!("\n[{who}] {}", t["text"].as_str().unwrap_or(""));
        for c in t["tool_calls"].as_array().into_iter().flatten() {
            out!(
                "  · {} {}",
                c["tool"].as_str().unwrap_or(""),
                c["arguments"]
            );
            if let Some(p) = c.get("proposal").filter(|p| p.is_object()) {
                out!(
                    "    {} {}: {}",
                    p["status"].as_str().unwrap_or(""),
                    c["action"].as_str().unwrap_or(""),
                    p["summary"].as_str().unwrap_or("")
                );
            }
        }
    }
    Ok(())
}

async fn decide(action: String, confirm: bool, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let d = if confirm {
        chat::actions::confirm(&f, &action).await?
    } else {
        chat::actions::reject(&f, &action)?
    };
    if json {
        out!(
            "{}",
            json!({
                "action": d.action, "status": d.status.as_str(),
                "outcome": d.outcome, "turn": d.turn,
            })
        );
    } else {
        out!("{} {}: {}", d.status.as_str(), d.action, d.outcome);
    }
    if d.status == chat::Status::Failed {
        bail!("action {} failed: {}", d.action, d.outcome);
    }
    Ok(())
}

pub(super) async fn dispatch(cmd: Cmd) -> Result<()> {
    let Cmd::Chat(c) = cmd else {
        unreachable!("command routed to the wrong family");
    };
    match c.cmd {
        None => say(c).await,
        Some(ChatSub::Sessions { limit, json }) => sessions(limit, json),
        Some(ChatSub::Show { id, json }) => show(id, json),
        Some(ChatSub::Confirm { action, json }) => decide(action, true, json).await,
        Some(ChatSub::Reject { action, json }) => decide(action, false, json).await,
    }
}
