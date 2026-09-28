//! Ask Forge (docs/CHAT.md): the operator's conversation with Forge about
//! its own state, the successor of Forge 1's ask channel. `forge chat` and
//! the web client's Chat page are two views of the same sessions.
//!
//! The model runs on the host through a provider whose runner is `chat`
//! (the operator's default provider when none is configured), one bounded
//! launch per step, with no tools of its own: it answers each step with a
//! JSON object that either names one of the fixed tools (`tools`) or
//! finishes with a reply. The tools are deterministic code over the store
//! and the CLI's JSON; the three that write only record a proposal, which
//! runs when the operator confirms it (`actions`). Every turn is recorded
//! in `chat_turns` with its tool calls, and its cost counts against the
//! per-day budget.

pub mod actions;
mod doc;
mod reads;
mod record;
mod redact;
mod step;
pub mod tools;

pub use doc::{session_doc, sessions_doc};
pub use record::{Status, ToolCall, action_id};

use crate::agent::{Provider, Runner};
use crate::ctx::Forge;
use crate::store::{ChatTurn, NewChatTurn};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

/// Tool calls one message may make before Ask Forge answers with what it
/// has.
const MAX_ROUNDS: usize = 8;
/// How much of the earlier conversation the model is shown.
const HISTORY_BYTES: usize = 24 * 1024;
/// How much of this message's tool results the model is shown.
const RESULTS_BYTES: usize = 32 * 1024;
const TITLE_CHARS: usize = 80;

/// What one message came to.
pub struct Asked {
    pub session: i64,
    /// The assistant turn recording the reply.
    pub turn: i64,
    pub reply: String,
    pub cost_usd: f64,
    pub calls: Vec<ToolCall>,
}

impl Asked {
    /// The write proposals the reply leaves for the operator: action id,
    /// tool, arguments and summary.
    pub fn proposals(&self) -> Vec<Value> {
        proposals_of(self.turn, &self.calls)
    }
}

pub(crate) fn proposals_of(turn: i64, calls: &[ToolCall]) -> Vec<Value> {
    calls
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let p = c.proposal.as_ref()?;
            Some(json!({
                "action": action_id(turn, i),
                "tool": c.tool,
                "arguments": c.arguments,
                "summary": p.summary,
                "status": p.status.as_str(),
                "outcome": p.outcome,
            }))
        })
        .collect()
}

/// The provider a chat runs under: the one asked for, else the first
/// configured whose runner is `chat`, else the operator's default for the
/// `plan` role.
pub fn pick_provider<'a>(f: &'a Forge, requested: Option<&str>) -> Result<&'a Provider> {
    if let Some(name) = requested {
        return f
            .providers
            .get(name)
            .with_context(|| format!("unknown provider {name:?}; see `forge providers`"));
    }
    if let Some(p) = f.providers.values().find(|p| p.runner == Runner::Chat) {
        return Ok(p);
    }
    crate::ctx::resolve_provider(
        &f.providers,
        &f.roles,
        &BTreeMap::new(),
        &BTreeMap::new(),
        "",
        "plan",
    )
}

/// The conversation so far as the model reads it, newest last, cut from
/// the front to `HISTORY_BYTES`.
fn history(turns: &[ChatTurn]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for t in turns {
        match t.role.as_str() {
            "user" => lines.push(format!("Operator: {}", t.text)),
            "assistant" => {
                let mut s = format!("Forge: {}", t.text);
                for (i, c) in record::parse_calls(&t.tool_calls).iter().enumerate() {
                    s.push_str(&format!("\n  (used {} {})", c.tool, c.arguments));
                    if let Some(p) = &c.proposal {
                        s.push_str(&format!(
                            "\n  (proposal {}: {} — {}{})",
                            action_id(t.id, i),
                            p.summary,
                            p.status.as_str(),
                            if p.outcome.is_empty() {
                                String::new()
                            } else {
                                format!(": {}", p.outcome)
                            }
                        ));
                    }
                }
                lines.push(s);
            }
            _ => lines.push(format!("[{}]", t.text)),
        }
    }
    let mut kept = Vec::new();
    let mut size = 0;
    for l in lines.iter().rev() {
        size += l.len() + 1;
        if size > HISTORY_BYTES && !kept.is_empty() {
            break;
        }
        kept.push(l.as_str());
    }
    kept.reverse();
    kept.join("\n")
}

/// This message's tool calls and what each returned, the newest kept in
/// full and the oldest dropped first once they pass `RESULTS_BYTES`.
fn results(calls: &[ToolCall]) -> String {
    let mut used = 0;
    let mut lines: Vec<String> = Vec::new();
    for (i, c) in calls.iter().enumerate().rev() {
        let outcome = match (&c.result, &c.error, &c.proposal) {
            (Some(r), _, _) => r.to_string(),
            (_, Some(e), _) => format!("error: {e}"),
            (_, _, Some(p)) => format!(
                "proposed (awaiting the operator's confirmation, nothing has happened): {}",
                p.summary
            ),
            _ => String::new(),
        };
        used += outcome.len();
        let outcome = if used > RESULTS_BYTES {
            "[dropped to keep the prompt short; call the tool again if you need it]".to_string()
        } else {
            outcome
        };
        lines.push(format!("{}. {} {} → {outcome}", i + 1, c.tool, c.arguments));
    }
    lines.reverse();
    lines.join("\n")
}

/// The user message of one model step.
fn step_prompt(turns: &[ChatTurn], message: &str, calls: &[ToolCall]) -> String {
    let past = history(turns);
    format!(
        "The conversation so far:\n{}\n\nThe operator's newest message:\n{message}\n\n\
         Tool calls you have made for this message, with their results:\n{}",
        if past.is_empty() {
            "(none: this is the first message)"
        } else {
            &past
        },
        if calls.is_empty() {
            "(none yet)".to_string()
        } else {
            results(calls)
        },
    )
}

fn title_of(message: &str) -> String {
    let one_line = message.split_whitespace().collect::<Vec<_>>().join(" ");
    reads::cut(&one_line, TITLE_CHARS)
}

/// Everything a step needs that does not change during a message.
struct Setup<'a> {
    provider: &'a Provider,
    model: String,
    system: String,
    schema: String,
    prompt_hash: String,
    timeout: Duration,
}

fn setup<'a>(f: &'a Forge, provider: &'a Provider) -> Result<Setup<'a>> {
    let actions = crate::workflows::load_actions(&f.paths.home)?;
    let chat = actions
        .get("chat")
        .context("the `chat` directive is missing from the library")?;
    let prompt = chat
        .prompt
        .as_deref()
        .context("the `chat` directive has no prompt")?;
    let system = format!(
        "{}\n\n{prompt}\n\n{}",
        crate::workflows::UNTRUSTED_DATA,
        tools::catalog()
    );
    Ok(Setup {
        provider,
        model: provider.model.clone().unwrap_or_else(|| "sonnet".into()),
        prompt_hash: crate::workflows::text_hash(&system),
        schema: chat
            .effective_schema()
            .context("the `chat` directive has no schema")?,
        timeout: Duration::from_secs(chat.timeout_secs.unwrap_or(180) as u64),
        system,
    })
}

/// Say `message` in `session` (a new one when `None`) and run it to a
/// reply. `emit` sees each event as it happens: the session and the
/// operator's turn, every tool call with its result or proposal, and the
/// reply.
pub async fn ask(
    f: &Forge,
    session: Option<i64>,
    message: &str,
    provider: Option<&str>,
    emit: &mut dyn FnMut(Value),
) -> Result<Asked> {
    let message = message.trim();
    if message.is_empty() {
        bail!("say something");
    }
    if let Some(why) = crate::worker::day_budget_reached(f)? {
        bail!("{why}; Ask Forge's own cost counts against it");
    }
    let provider = pick_provider(f, provider)?;
    if let Some((why, _)) = crate::worker::window_hold(f, &provider.name)? {
        bail!("provider {} is held: {why}", provider.name);
    }
    let setup = setup(f, provider)?;
    let session = match session {
        Some(id) => {
            f.store
                .chat_session(id)?
                .with_context(|| format!("no chat session {id}"))?;
            id
        }
        None => f
            .store
            .create_chat_session(&title_of(message), &provider.name)?,
    };
    let earlier = f.store.chat_turns(session)?;
    let user_turn = f.store.insert_chat_turn(&NewChatTurn {
        session,
        role: "user",
        text: message,
        tool_calls: "[]",
        cost_usd: 0.0,
        provider: "",
        model: "",
        prompt_hash: "",
    })?;
    emit(json!({"type": "session", "session": session}));
    emit(json!({"type": "turn", "turn": user_turn, "role": "user", "text": message}));
    let red = tools::redactor(f);
    let run = step::converse(f, &setup, &red, &earlier, message, user_turn, emit).await;
    let (reply, calls, cost, failure) = match run {
        Ok(r) => (r.reply, r.calls, r.cost_usd, None),
        Err(fail) => (
            format!("(Ask Forge could not answer: {:#})", fail.error),
            fail.calls,
            fail.cost_usd,
            Some(fail.error),
        ),
    };
    let reply = red.text(&reply);
    let turn = f.store.insert_chat_turn(&NewChatTurn {
        session,
        role: "assistant",
        text: &reply,
        tool_calls: &record::calls_json(&calls),
        cost_usd: cost,
        provider: &provider.name,
        model: &setup.model,
        prompt_hash: &setup.prompt_hash,
    })?;
    let asked = Asked {
        session,
        turn,
        reply,
        cost_usd: cost,
        calls,
    };
    match failure {
        Some(e) => {
            emit(
                json!({"type": "error", "session": session, "turn": turn, "message": format!("{e:#}")}),
            );
            Err(e)
        }
        None => {
            emit(json!({
                "type": "reply", "session": session, "turn": turn, "text": asked.reply,
                "cost_usd": cost, "proposals": asked.proposals(),
            }));
            Ok(asked)
        }
    }
}
