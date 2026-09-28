//! The read-only tools: each is a query over the store or the CLI's own
//! JSON documents (`view`), never a shell and never a file read outside
//! `FORGE_HOME/logs`. What they return is a whitelisted, bounded copy of
//! the document, so the model gets what answers the question and not the
//! task's whole record.

use crate::ctx::Forge;
use crate::store::{DecisionFilter, TaskFilter, TaskState};
use crate::view;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

/// The most rows a list tool returns, whatever the model asks for.
const MAX_ROWS: usize = 40;
const DEFAULT_ROWS: usize = 15;
/// A string longer than this is cut in a tool result.
const MAX_TEXT: usize = 1200;
/// How much of a log the `log_tail` tool returns.
const LOG_TAIL_LINES: usize = 40;
const LOG_TAIL_BYTES: u64 = 64 * 1024;
const LOG_LINE_CHARS: usize = 400;

pub fn int(args: &Value, key: &str) -> Result<i64> {
    args.get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok()))
        .with_context(|| format!("`{key}` is required and is a whole number"))
}

pub fn opt_int(args: &Value, key: &str) -> Result<Option<i64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => int(args, key).map(Some),
    }
}

pub fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn rows(args: &Value) -> usize {
    (opt_int(args, "limit")
        .ok()
        .flatten()
        .unwrap_or(DEFAULT_ROWS as i64)
        .max(1) as usize)
        .min(MAX_ROWS)
}

pub fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}… [cut]")
}

/// `v` with every string cut to `max` characters and every array to
/// `MAX_ROWS` items.
fn shrink(v: &Value, max: usize) -> Value {
    match v {
        Value::String(s) => Value::String(cut(s, max)),
        Value::Array(a) => Value::Array(a.iter().take(MAX_ROWS).map(|i| shrink(i, max)).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, v)| (k.clone(), shrink(v, max))).collect())
        }
        other => other.clone(),
    }
}

/// `v` cut down until its JSON fits `budget` bytes: strings first, ever
/// shorter, and as a last resort the text of what would not fit.
pub fn bound(v: Value, budget: usize) -> Value {
    let mut max = MAX_TEXT;
    let mut v = shrink(&v, max);
    while v.to_string().len() > budget && max > 40 {
        max /= 2;
        v = shrink(&v, max);
    }
    if v.to_string().len() > budget {
        return json!({"truncated": cut(&v.to_string(), budget)});
    }
    v
}

/// The keys of `v` named in `keep`, in that order.
fn pick(v: &Value, keep: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for k in keep {
        if let Some(x) = v.get(*k)
            && !x.is_null()
        {
            out.insert((*k).to_string(), x.clone());
        }
    }
    Value::Object(out)
}

fn to_value<T: serde::Serialize>(t: &T) -> Result<Value> {
    serde_json::to_value(t).context("serializing a document")
}

/// An attempt as the model needs it: what ran, how it ended, and the
/// checks that failed with their first lines.
fn attempt_summary(a: &Value) -> Value {
    let mut out = pick(
        a,
        &[
            "attempt_no",
            "step",
            "state",
            "reason",
            "started_at",
            "finished_at",
            "timed_out",
            "num_turns",
            "cost_usd",
            "provider",
            "commits",
            "files_changed",
            "log_path",
        ],
    );
    let failed: Vec<Value> = a["verdict"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["ok"] == false)
        .map(|r| {
            json!({
                "level": r["level"],
                "check": r["name"],
                "exit": r["exit"],
                "tail": cut(r["tail"].as_str().unwrap_or_default(), 500),
                "failing_tests": r["failing_tests"],
            })
        })
        .collect();
    out["failed_checks"] = Value::Array(failed);
    let env = &a["envelope"];
    if env.is_object() {
        out["summary"] = env["summary"].clone();
        out["needs_input"] = env["needs_input"].clone();
    }
    out
}

const TASK_KEYS: &[&str] = &[
    "id",
    "repo",
    "text",
    "state",
    "trust",
    "reason",
    "workflow",
    "provider",
    "model",
    "max_attempts",
    "budget_usd",
    "land",
    "pushed",
    "after",
    "retry_of",
    "children",
    "root",
    "project",
    "initiative",
    "checks",
    "plan",
    "to",
    "created_at",
    "started_at",
    "finished_at",
    "live_descendants",
];

/// `task {id}`: the task, its attempts (each with its failed checks), its
/// operations, and the kernel's diagnosis.
pub fn task(f: &Forge, args: &Value) -> Result<Value> {
    let id = int(args, "id")?;
    let t = f.store.task(id)?.ok_or_else(|| anyhow!("no task {id}"))?;
    let doc = to_value(&view::trace_doc(f, &t)?)?;
    let ops: Vec<Value> = doc["ops"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| pick(o, &["name", "ok", "exit", "detail", "attempt_id"]))
        .collect();
    Ok(json!({
        "task": pick(&doc["task"], TASK_KEYS),
        "attempts": doc["attempts"].as_array().into_iter().flatten().map(attempt_summary).collect::<Vec<_>>(),
        "ops": ops,
        "diagnosis": doc["diagnosis"],
        "assessment": doc["assessment"],
    }))
}

/// `attempt {task, attempt_no}`: one attempt whole (its checks' output
/// and the agent's own summary and claims).
pub fn attempt(f: &Forge, args: &Value) -> Result<Value> {
    let id = int(args, "task")?;
    let no = int(args, "attempt_no")?;
    let t = f.store.task(id)?.ok_or_else(|| anyhow!("no task {id}"))?;
    let doc = to_value(&view::trace_doc(f, &t)?)?;
    let a = doc["attempts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["attempt_no"].as_i64() == Some(no))
        .ok_or_else(|| anyhow!("task {id} has no attempt {no}"))?;
    let mut out = attempt_summary(a);
    out["tokens"] = a["tokens"].clone();
    out["agent_ms"] = a["agent_ms"].clone();
    out["verdict"] = a["verdict"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| json!({"level": r["level"], "check": r["name"], "ok": r["ok"], "exit": r["exit"]}))
        .collect();
    out["claims"] = a["envelope"]["claims"].clone();
    out["changes"] = a["envelope"]["changes"].clone();
    Ok(out)
}

/// `tasks {state?, project?, initiative?, grep?, limit?}`: `forge log`.
pub fn tasks(f: &Forge, args: &Value) -> Result<Value> {
    let state = match opt_str(args, "state") {
        Some(s) => Some(TaskState::try_from(s).map_err(|e| anyhow!("`state`: {e}"))?),
        None => None,
    };
    let q = TaskFilter {
        limit: rows(args) as u32,
        state,
        project: opt_str(args, "project").map(str::to_string),
        initiative: opt_int(args, "initiative")?,
        grep: opt_str(args, "grep").map(str::to_string),
        workflow: opt_str(args, "workflow").map(str::to_string),
        before: opt_int(args, "before")?,
        ..Default::default()
    };
    let list: Vec<Value> = view::task_rows(f, &q)?
        .iter()
        .map(|r| {
            let v = to_value(r)?;
            Ok(pick(
                &v,
                &[
                    "id",
                    "state",
                    "workflow",
                    "attempts",
                    "cost_usd",
                    "created_at",
                    "finished_at",
                    "project",
                    "initiative",
                    "task",
                ],
            ))
        })
        .collect::<Result<_>>()?;
    Ok(json!({"tasks": list}))
}

/// `initiative {id}`: what it is for, its state and hold, each lineage's
/// latest task, its open questions and rulings.
pub fn initiative(f: &Forge, args: &Value) -> Result<Value> {
    let id = int(args, "id")?;
    let ini = f
        .store
        .initiative(id)?
        .ok_or_else(|| anyhow!("no initiative {id}"))?;
    to_value(&view::initiative_doc(f, &ini)?)
}

/// `initiatives {project?}`: one row each, with counts by state.
pub fn initiatives(f: &Forge, args: &Value) -> Result<Value> {
    let rows = view::initiative_rows(f, opt_str(args, "project"))?;
    Ok(json!({"initiatives": to_value(&rows)?}))
}

/// `decisions {task?, grep?, limit?}`: `forge decisions`, newest first.
pub fn decisions(f: &Forge, args: &Value) -> Result<Value> {
    let want = opt_int(args, "task")?;
    let all = f.store.decisions(&DecisionFilter {
        grep: opt_str(args, "grep").map(str::to_string),
        ..Default::default()
    })?;
    let list: Vec<Value> = all
        .iter()
        .filter(|d| want.is_none() || d.task_id == want)
        .take(rows(args))
        .map(|d| {
            let outcome = d
                .retry_id
                .and_then(|r| f.store.task(r).ok().flatten())
                .map(|t| t.state);
            to_value(&view::DecisionRow::new(d, outcome))
        })
        .collect::<Result<_>>()?;
    Ok(json!({"decisions": list}))
}

/// `requests {}`: every task blocked on a question, and what it asks.
pub fn requests(f: &Forge, _args: &Value) -> Result<Value> {
    let list: Vec<Value> = f
        .store
        .blocked(None, None)?
        .iter()
        .take(MAX_ROWS)
        .map(|t| {
            let (kind, text) = view::request_kind(&t.reason);
            json!({
                "task": t.id, "kind": kind, "waiting_on": text,
                "to": t.question_to, "project": t.project, "initiative": t.initiative,
                "created_at": t.created_at,
            })
        })
        .collect();
    Ok(json!({"requests": list}))
}

/// `doctor {}`: `forge doctor --json`, without the checks that are fine.
pub fn doctor(f: &Forge, _args: &Value) -> Result<Value> {
    let checks = crate::doctor::run_at(f.paths.clone())?;
    let all = to_value(&checks)?;
    let (ok, wrong): (Vec<&Value>, Vec<&Value>) = all
        .as_array()
        .into_iter()
        .flatten()
        .partition(|c| c["status"] == "ok");
    Ok(json!({
        "ok": ok.iter().map(|c| c["name"].clone()).collect::<Vec<_>>(),
        "not_ok": wrong,
    }))
}

/// `stats {days?}`: spend against the per-day cap, the last days' landings
/// and cost, and the per-workflow and per-role figures `forge stats`
/// leads with — from the store alone.
pub fn stats(f: &Forge, args: &Value) -> Result<Value> {
    let days = opt_int(args, "days")?.unwrap_or(7).clamp(1, 30) as usize;
    let now = crate::unix_now();
    let scope = crate::store::StatsFilter::default();
    let daily: Vec<view::StatsDailyRow> = f
        .store
        .daily_stats(&scope)?
        .iter()
        .map(Into::into)
        .collect();
    let recent = &daily[daily.len().saturating_sub(days)..];
    let workflows: Vec<view::StatsWorkflowRow> = f
        .store
        .workflow_stats(&scope)?
        .iter()
        .map(Into::into)
        .collect();
    let by_role: Vec<view::StatsRoleRow> = f.store.role_stats()?.iter().map(Into::into).collect();
    Ok(json!({
        "spent_last_24h_usd": f.store.spent_since(now - 86_400)?,
        "per_day_cap_usd": f.budget.per_day_usd,
        "daily": to_value(&recent)?,
        "workflows": to_value(&workflows)?,
        "by_role": to_value(&by_role)?,
    }))
}

/// `log_tail {task, attempt_no?, lines?}`: the last lines of an attempt's
/// log under `FORGE_HOME/logs`, the file `forge trace` names. The path is
/// the store's, and must resolve inside the logs directory.
pub fn log_tail(f: &Forge, args: &Value) -> Result<Value> {
    use std::io::{Read, Seek, SeekFrom};
    let id = int(args, "task")?;
    let attempts = f.store.attempts(id)?;
    let a = match opt_int(args, "attempt_no")? {
        Some(no) => attempts.iter().find(|a| a.attempt_no == no),
        None => attempts.last(),
    }
    .ok_or_else(|| anyhow!("task {id} has no such attempt"))?;
    if a.log_path.is_empty() {
        bail!("that attempt has no log");
    }
    let logs = f.paths.logs.canonicalize().context("the logs directory")?;
    let path = std::path::Path::new(&a.log_path)
        .canonicalize()
        .context("that attempt's log is gone")?;
    if !path.starts_with(&logs) {
        bail!("that attempt's log is not under FORGE_HOME/logs");
    }
    let mut file = std::fs::File::open(&path).context("opening the log")?;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(LOG_TAIL_BYTES)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let want = (opt_int(args, "lines")?.unwrap_or(20).max(1) as usize).min(LOG_TAIL_LINES);
    let lines: Vec<String> = text
        .lines()
        .rev()
        .take(want)
        .map(|l| cut(l, LOG_LINE_CHARS))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Ok(json!({"task": id, "attempt_no": a.attempt_no, "lines": lines}))
}

/// `events_since {cursor?, since_ts?, task?, limit?}`: the event log after
/// a cursor (or a unix time), newest last; `next` continues from here.
pub fn events_since(f: &Forge, args: &Value) -> Result<Value> {
    use crate::report::log::{self, Cursor};
    let path = f.paths.home.join("events.jsonl");
    let from: Cursor = match opt_str(args, "cursor") {
        Some(c) => c
            .parse()
            .map_err(|_| anyhow!("`cursor` is generation:offset"))?,
        None => Cursor::default(),
    };
    let since_ts = opt_int(args, "since_ts")?;
    let task = opt_int(args, "task")?;
    let batch = log::read(&path, from, 8 * 1024 * 1024).context("reading the event log")?;
    let mut events: Vec<Value> = batch
        .lines
        .iter()
        .filter_map(|(_, _, line)| serde_json::from_str::<Value>(line).ok())
        .filter(|e| e.get("type").is_some())
        .filter(|e| task.is_none() || e["task"].as_i64() == task)
        .filter(|e| since_ts.is_none_or(|t| e["ts"].as_i64().unwrap_or(0) >= t))
        .collect();
    let keep = rows(args);
    events.drain(..events.len().saturating_sub(keep));
    Ok(json!({"events": events, "next": batch.next.to_string()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_cuts_strings_until_the_document_fits() {
        let long = "y".repeat(5000);
        let v = json!({"a": "x".repeat(5000), "b": [long, long, long, long, long]});
        let out = bound(v, 2000);
        assert!(out.to_string().len() <= 2000, "{}", out.to_string().len());
        assert!(out.to_string().contains("[cut]"));
    }

    #[test]
    fn a_document_that_already_fits_is_untouched() {
        let v = json!({"a": 1, "b": ["x"]});
        assert_eq!(bound(v.clone(), 2000), v);
    }

    #[test]
    fn int_takes_a_number_or_a_numeral_and_names_what_is_missing() {
        assert_eq!(int(&json!({"id": 7}), "id").unwrap(), 7);
        assert_eq!(int(&json!({"id": " 7 "}), "id").unwrap(), 7);
        let e = int(&json!({}), "id").unwrap_err().to_string();
        assert!(e.contains("`id`"), "{e}");
    }

    #[test]
    fn row_limits_are_clamped() {
        assert_eq!(rows(&json!({"limit": 9999})), MAX_ROWS);
        assert_eq!(rows(&json!({"limit": 0})), 1);
        assert_eq!(rows(&json!({})), DEFAULT_ROWS);
    }
}
