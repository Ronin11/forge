//! One message's run: launch the model, read what it says, run the tool it
//! names, and go again until it finishes or the calls run out. The
//! model's words are parsed leniently — the schema is asked for, but a
//! provider that ignores it still gets its reply read — and never
//! trusted: a tool name is looked up in `tools::TOOLS`, its arguments are
//! checked by the tool, and a write only becomes a proposal.

use super::record::ToolCall;
use super::redact::Redactor;
use super::tools::{self, Called};
use super::{MAX_ROUNDS, Setup, step_prompt};
use crate::ctx::Forge;
use crate::store::ChatTurn;
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

/// A finished message.
pub struct Converse {
    pub reply: String,
    pub calls: Vec<ToolCall>,
    pub cost_usd: f64,
}

/// A message that could not finish: what was done and spent before it
/// failed is still recorded.
pub struct Failed {
    pub error: anyhow::Error,
    pub calls: Vec<ToolCall>,
    pub cost_usd: f64,
}

/// What the model said in one step: a sentence, and the tools it wants.
#[derive(Debug, Default, PartialEq)]
pub struct Step {
    pub reply: String,
    pub calls: Vec<(String, Value)>,
}

const REPLY_KEYS: &[&str] = &[
    "reply", "answer", "text", "message", "content", "say", "response",
];
const CALL_LISTS: &[&str] = &["tool_calls", "calls", "actions", "proposals", "tools"];
const NAME_KEYS: &[&str] = &["tool", "name", "tool_name", "action"];
const ARG_KEYS: &[&str] = &["arguments", "args", "input", "parameters"];

/// A model that fences its JSON in ``` still gets read.
fn unfence(raw: &str) -> &str {
    let t = raw.trim();
    let Some(body) = t.strip_prefix("```") else {
        return t;
    };
    let body = body.split_once('\n').map_or(body, |(_, rest)| rest);
    body.strip_suffix("```").unwrap_or(body).trim()
}

fn arguments_of(item: &Value) -> Value {
    for k in ARG_KEYS {
        match item.get(*k) {
            Some(Value::String(s)) => {
                if let Ok(v) = serde_json::from_str::<Value>(s) {
                    return v;
                }
            }
            Some(v) if !v.is_null() => return v.clone(),
            _ => {}
        }
    }
    json!({})
}

fn call_of(item: &Value) -> Option<(String, Value)> {
    let name = NAME_KEYS
        .iter()
        .find_map(|k| item.get(*k).and_then(Value::as_str))
        .or_else(|| item["function"]["name"].as_str())?
        .trim();
    if name.is_empty() || matches!(name, "none" | "null" | "finish" | "final") {
        return None;
    }
    let args = if item.get("function").is_some() && item.get("arguments").is_none() {
        arguments_of(&item["function"])
    } else {
        arguments_of(item)
    };
    Some((name.to_string(), args))
}

/// Read a model's answer: the structured result when there is one, else
/// the text. A reply that is not a JSON object is a finished reply of
/// plain words.
pub fn parse_step(raw: &str) -> Step {
    let v: Option<Value> = serde_json::from_str(unfence(raw))
        .ok()
        .filter(Value::is_object);
    let Some(v) = v else {
        return Step {
            reply: raw.trim().to_string(),
            calls: Vec::new(),
        };
    };
    let reply = REPLY_KEYS
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .unwrap_or_default()
        .trim()
        .to_string();
    let listed = CALL_LISTS
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_array));
    let calls = match listed {
        Some(items) => items.iter().filter_map(call_of).collect(),
        None => call_of(&v).into_iter().collect(),
    };
    Step { reply, calls }
}

/// One launch of the model.
async fn model_step(
    f: &Forge,
    s: &Setup<'_>,
    prompt: &str,
    scratch: &std::path::Path,
    log: &std::path::Path,
) -> Result<(Step, f64)> {
    let o = crate::directive::launch(
        f,
        crate::directive::Spec {
            id: 0,
            step: "chat",
            dir: scratch,
            prompt,
            system: &s.system,
            model: &s.model,
            max_turns: 1,
            timeout: s.timeout,
            check_timeout: std::time::Duration::ZERO,
            log_path: log,
            provider: s.provider,
            schema: &s.schema,
            sandboxed: false,
            writes: false,
            start_sha: "",
            resume: None,
            no_tools: true,
            judgment: None,
        },
    )
    .await?;
    let cost = o.cost_usd.unwrap_or(0.0);
    if let Some(why) = crate::directive::failure(&o) {
        let tail = crate::checks::last_lines(&o.stderr_text, 20);
        return Err(anyhow!("{}", why.tail(&tail))).context(format!("(cost so far ${cost:.4})"));
    }
    let raw = o
        .structured
        .clone()
        .unwrap_or_else(|| o.result_text.clone());
    Ok((parse_step(&raw), cost))
}

/// Run one call the model asked for, as the record keeps it.
fn run_call(
    f: &Forge,
    red: &Redactor,
    name: &str,
    args: &Value,
    note: &str,
    seen: &[ToolCall],
) -> ToolCall {
    if seen.iter().any(|c| c.tool == name && &c.arguments == args) {
        return ToolCall::failed(
            name,
            args.clone(),
            note,
            "you already made this exact call for this message; its result is above".into(),
        );
    }
    match tools::call(f, red, name, args) {
        Ok(Called::Answer(v)) => ToolCall::read(name, args.clone(), note, v),
        Ok(Called::Proposal { arguments, summary }) => {
            ToolCall::proposed(name, arguments, note, summary)
        }
        Err(e) => ToolCall::failed(name, args.clone(), note, red.text(&format!("{e:#}"))),
    }
}

/// The event a finished call is announced by.
fn call_event(c: &ToolCall) -> Value {
    json!({
        "type": "tool", "tool": c.tool, "arguments": c.arguments, "note": c.note,
        "result": c.result, "error": c.error,
        "proposal": c.proposal.as_ref().map(|p| json!({"summary": p.summary})),
    })
}

pub async fn converse(
    f: &Forge,
    s: &Setup<'_>,
    red: &Redactor,
    earlier: &[ChatTurn],
    message: &str,
    user_turn: i64,
    emit: &mut dyn FnMut(Value),
) -> std::result::Result<Converse, Failed> {
    let mut calls: Vec<ToolCall> = Vec::new();
    let mut cost = 0.0;
    let mut last_note = String::new();
    let scratch = tempfile::tempdir_in(&f.paths.home)
        .context("making a scratch directory")
        .map_err(|error| Failed {
            error,
            calls: Vec::new(),
            cost_usd: 0.0,
        })?;
    for round in 0..MAX_ROUNDS {
        let prompt = step_prompt(earlier, message, &calls);
        let log = f.paths.logs.join(format!("chat-{user_turn}-{round}.jsonl"));
        let (step, spent) = match model_step(f, s, &prompt, scratch.path(), &log).await {
            Ok(r) => r,
            Err(error) => {
                return Err(Failed {
                    error,
                    calls,
                    cost_usd: cost,
                });
            }
        };
        cost += spent;
        if step.calls.is_empty() {
            let reply = if step.reply.is_empty() {
                last_note
            } else {
                step.reply
            };
            if reply.is_empty() {
                let error = anyhow!("the model returned nothing");
                return Err(Failed {
                    error,
                    calls,
                    cost_usd: cost,
                });
            }
            return Ok(Converse {
                reply,
                calls,
                cost_usd: cost,
            });
        }
        last_note = step.reply.clone();
        for (name, args) in &step.calls {
            let call = run_call(f, red, name, args, &step.reply, &calls);
            emit(call_event(&call));
            calls.push(call);
        }
    }
    let reply = if last_note.is_empty() {
        format!(
            "I made {MAX_ROUNDS} rounds of tool calls without reaching an answer; the calls are recorded above."
        )
    } else {
        format!("{last_note} (I stopped after {MAX_ROUNDS} rounds of tool calls.)")
    };
    Ok(Converse {
        reply,
        calls,
        cost_usd: cost,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_structured_step_with_a_tool_is_a_call() {
        let s = parse_step(r#"{"reply":"looking","tool":"task","arguments":{"id":903}}"#);
        assert_eq!(s.reply, "looking");
        assert_eq!(s.calls, vec![("task".to_string(), json!({"id": 903}))]);
    }

    #[test]
    fn an_empty_tool_finishes_with_the_reply() {
        let s = parse_step(r#"{"reply":"It failed twice.","tool":"","arguments":{}}"#);
        assert_eq!(s.reply, "It failed twice.");
        assert!(s.calls.is_empty());
    }

    #[test]
    fn other_shapes_a_provider_might_use_are_read() {
        let s = parse_step(
            r#"{"answer":"ok","tool_calls":[{"function":{"name":"retry_task","arguments":"{\"task\":5}"}},{"name":"task","args":{"id":1}}]}"#,
        );
        assert_eq!(s.reply, "ok");
        assert_eq!(
            s.calls,
            vec![
                ("retry_task".to_string(), json!({"task": 5})),
                ("task".to_string(), json!({"id": 1})),
            ]
        );
        let fenced = parse_step("```json\n{\"reply\":\"hi\",\"tool\":\"none\"}\n```");
        assert_eq!(fenced.reply, "hi");
        assert!(fenced.calls.is_empty());
    }

    #[test]
    fn plain_words_are_a_finished_reply() {
        let s = parse_step("  Task 903 failed on clippy.  ");
        assert_eq!(s.reply, "Task 903 failed on clippy.");
        assert!(s.calls.is_empty());
    }
}
