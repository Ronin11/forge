//! Notification policy belongs to the kernel, after automatic recovery.
use crate::ctx::Forge;
use crate::report::Event;
use crate::store::{Store, Task, TaskState};
use anyhow::Result;
use std::path::Path;

/// A recorded follow-up answers the original transition even if that
/// follow-up has since ended: its own transition carries the next question.
fn answered(store: &Store, id: i64) -> Result<bool> {
    if !store.live_descendants(id)?.is_empty() {
        return Ok(true);
    }
    for d in store.decisions_in_lineage(id)? {
        if d.task_id == Some(id)
            && let Some(next) = d.retry_id
            && store.task(next)?.is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Classify the transition, using the current record after recovery.
/// A contact question and an operator question have the same audience;
/// delivery uses question_to to choose the person.
pub(crate) fn classify(store: &Store, ended: &Task) -> Result<&'static str> {
    if !matches!(ended.state, TaskState::Blocked | TaskState::Failed) {
        return Ok("none");
    }
    if answered(store, ended.id)? {
        return Ok("none");
    }
    let current = store.task(ended.id)?.unwrap_or_else(|| ended.clone());
    if !matches!(current.state, TaskState::Blocked | TaskState::Failed) {
        return Ok("none");
    }
    if current.reason.starts_with("waits on task") && !current.after.is_empty() {
        for dep in &current.after {
            let live = store.task(*dep)?.is_some_and(|t| {
                matches!(t.state, TaskState::Queued | TaskState::Running)
                    || t.state == TaskState::Withdrawn
                    || (t.state == TaskState::Succeeded && (!t.land || !t.landed_sha.is_empty()))
            });
            if !live && !live_followup(store, *dep)? {
                return Ok("person");
            }
        }
        return Ok("none");
    }
    Ok("person")
}

pub(crate) fn emit(f: &Forge, t: &Task, compare: Option<&str>, wt: &Path) -> Result<()> {
    f.report.emit(
        t.id,
        Event::TaskDone {
            audience: classify(&f.store, t)?,
            state: t.state.as_str(),
            attempts: f.store.attempts(t.id)?.len(),
            cost: f.store.task_cost(t.id)?,
            reason: &t.reason,
            branch: &t.branch,
            pushed: t.pushed,
            compare,
            remove_cmd: &format!("rm -rf {}", wt.display()),
        },
    );
    Ok(())
}

pub(crate) fn emit_ended(f: &Forge, t: &Task) -> Result<()> {
    let wt = f.paths.worktrees.join(t.id.to_string());
    emit(f, t, None, &wt)
}

pub(crate) async fn emit_recovered(f: &Forge, t: &Task) -> Result<()> {
    let compare = crate::git::remote_url(Path::new(&t.repo), "origin")
        .await
        .and_then(|url| crate::git::compare_url(&url, &t.base_branch, &t.branch));
    emit(
        f,
        t,
        compare.as_deref(),
        &f.paths.worktrees.join(t.id.to_string()),
    )
}

/// The daily job reads durable records, so downtime and plugin restarts do
/// not lose counts. Days use UTC, like the kernel's other daily statistics.
pub(crate) fn daily_digest(f: &Forge) -> Result<()> {
    let today = crate::unix_now().div_euclid(86_400) * 86_400;
    let text = f.store.notification_digest(today - 86_400, today)?;
    f.report.emit(
        0,
        Event::NotificationDigest {
            day: today - 86_400,
            text: &text,
        },
    );
    Ok(())
}

fn live_followup(store: &Store, id: i64) -> Result<bool> {
    if !store.live_descendants(id)?.is_empty() {
        return Ok(true);
    }
    for d in store.decisions_in_lineage(id)? {
        if d.task_id == Some(id)
            && let Some(next) = d.retry_id.filter(|next| *next != id)
            && let Some(t) = store.task(next)?
            && matches!(
                t.state,
                TaskState::Queued | TaskState::Running | TaskState::Unverified
            )
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests;
