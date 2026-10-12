//! Alerts become work (docs/research/06-one-person-factories.md, 2.1).
//! A watch that trips ERROR, or WARN with a hint that names a concrete
//! repair, opens one task in the `forge` project keyed by the watch name
//! and holds it while the condition stands: at most one open task per
//! key, a new one if it closed while the condition still stands (looked
//! at most every six hours), and the key marked handled when the watch
//! goes OK. The task's text is the watch's name, detail and hint, all
//! written by Forge; nothing a model wrote is in it.
//!
//! What it remembers is one `doctor_marks` row per key, `alert.<watch>`:
//! status `open` or `handled`, the task's id as the detail, and when the
//! task was opened or last looked at.

use super::{Check, Status};
use crate::ctx::Forge;
use crate::queue::{self, TaskRequest};
use crate::store::{InsertDecisionBy, Mark, TaskState};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

/// The project whose queue takes the tasks.
pub const PROJECT: &str = "forge";

/// The WARN rows whose hint names a concrete repair; every ERROR row
/// qualifies on its own.
pub const REPAIRABLE: &[&str] = &[
    "jobs_stale",
    "worker_restarts",
    "egress_model",
    "supervisor",
    "land_rate",
];

/// How often an open key's task is looked at for having been closed.
pub const RECHECK_SECS: i64 = 6 * 3600;

/// The decision `kind` of an opening, for `forge stats`.
pub fn decision_kind(watch: &str) -> String {
    format!("alert-{watch}")
}

fn mark_name(watch: &str) -> String {
    format!("alert.{watch}")
}

/// Whether `row` is a condition that is work.
pub fn is_work(row: &Check) -> bool {
    match row.status {
        Status::Fail => true,
        Status::Warn => REPAIRABLE.contains(&row.name.as_str()),
        Status::Ok => false,
    }
}

/// The task's text: the watch, its detail and its hint.
pub fn task_text(row: &Check) -> String {
    let status = match row.status {
        Status::Fail => "ERROR",
        _ => "WARN",
    };
    let mut text = format!(
        "The doctor watch `{}` is {status} and still standing.\n\nDetail: {}\n",
        row.name, row.detail
    );
    if !row.hint.is_empty() {
        text.push_str(&format!("\nHint: {}\n", row.hint));
    }
    text.push_str("\nRepair the condition; the watch going OK closes this.\n");
    text
}

fn is_open(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Queued | TaskState::Running | TaskState::Blocked | TaskState::Capped
    )
}

/// The `forge` project's first repository that exists on disk.
fn repo(f: &Forge) -> Result<PathBuf> {
    f.store
        .project_repos(PROJECT)?
        .into_iter()
        .map(|r| PathBuf::from(r.repo))
        .find(|p| p.join(".git").exists())
        .with_context(|| format!("project {PROJECT} has no repository on disk"))
}

/// File the task for `row` and record the opening as a decision.
async fn open(f: &Forge, row: &Check) -> Result<i64> {
    let req = TaskRequest {
        repo: repo(f)?,
        task: task_text(row),
        project: Some(PROJECT.into()),
        max_turns: 100,
        retries: 1,
        timeout_secs: 1800,
        ..Default::default()
    };
    let t = queue::enqueue(f, &req, None).await?;
    let id = f.store.insert_decision_by(InsertDecisionBy {
        task_id: t.id,
        repo: &t.repo,
        question: &format!("doctor watch {}: {}", row.name, row.detail),
        answer: &format!("opened task {}", t.id),
        answered_by: "doctor",
        citations: "",
        answered_for: None,
    })?;
    f.store.set_decision_kind(id, &decision_kind(&row.name))?;
    Ok(t.id)
}

fn remember(f: &Forge, watch: &str, status: &str, task: i64, now: i64) -> Result<()> {
    f.store.set_doctor_mark(
        &mark_name(watch),
        &Mark {
            status: status.into(),
            detail: task.to_string(),
            at: now,
        },
    )
}

/// Bring the tasks in line with `rows`. Returns the ids of the tasks
/// opened. A pass that changes nothing writes nothing.
pub async fn sync(f: &Forge, rows: &[Check], now: i64) -> Result<Vec<i64>> {
    let mut opened = Vec::new();
    let mut failed = Vec::new();
    for row in rows {
        if let Err(e) = sync_one(f, row, now, &mut opened).await {
            failed.push(format!("{}: {e:#}", row.name));
        }
    }
    if !failed.is_empty() {
        bail!("alert tasks: {}", failed.join("; "));
    }
    Ok(opened)
}

async fn sync_one(f: &Forge, row: &Check, now: i64, opened: &mut Vec<i64>) -> Result<()> {
    let mark = f.store.doctor_mark(&mark_name(&row.name))?;
    let standing = mark.as_ref().filter(|m| m.status == "open");
    if row.status == Status::Ok {
        let Some(m) = standing else { return Ok(()) };
        let task: i64 = m.detail.parse().unwrap_or(0);
        // The condition cleared on its own: a task nobody has started is
        // moot.
        if let Some(t) = f.store.task(task)?
            && matches!(t.state, TaskState::Queued | TaskState::Blocked)
        {
            let _ = queue::withdraw(f, task, "the watch went OK", "doctor");
        }
        return remember(f, &row.name, "handled", task, now);
    }
    if !is_work(row) {
        return Ok(());
    }
    if let Some(m) = standing {
        if now - m.at < RECHECK_SECS {
            return Ok(());
        }
        let task: i64 = m.detail.parse().unwrap_or(0);
        if f.store.task(task)?.is_some_and(|t| is_open(t.state)) {
            return remember(f, &row.name, "open", task, now);
        }
    }
    let id = open(f, row).await?;
    remember(f, &row.name, "open", id, now)?;
    opened.push(id);
    Ok(())
}

/// The worker's end-of-pass doctor: every watch, then the tasks they owe.
/// Failures are the caller's to log, never the pass's.
pub async fn tick(f: &Forge) -> Result<Vec<i64>> {
    let now = crate::unix_now();
    let rows = super::watch::tick(&f.paths, &f.store, now);
    sync(f, &rows, now).await
}

#[cfg(test)]
mod tests;
