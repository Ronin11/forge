//! The running system: claim, run, repeat. `drive` is the one place a
//! fault is turned into an outcome: a task fault fails the task; an
//! environment fault puts the task back in the queue and stops the worker.

use crate::ctx::Forge;
use crate::engine::{self, Fault};
use crate::report::Event;
use crate::store::TaskState;
use crate::unix_now;
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

pub fn pid_alive(pid: i64) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Run one claimed task to its end. `Err` means the worker environment is
/// broken; the task has already been requeued.
pub async fn drive(f: Arc<Forge>, id: i64) -> Result<TaskState> {
    match engine::run_task(f.clone(), id).await {
        Ok(state) => Ok(state),
        Err(Fault::Task(e)) => {
            f.report.emit(
                id,
                Event::Note {
                    text: &format!("ERROR    {e:#}"),
                },
            );
            if let Some(mut t) = f.store.task(id)? {
                t.state = TaskState::Failed;
                t.reason = format!("error: {e:#}");
                t.finished_at = Some(unix_now());
                t.worker_pid = None;
                f.store.update_task(&t)?;
            }
            Ok(TaskState::Failed)
        }
        Err(Fault::Env(e)) => {
            f.store.requeue(id, "worker environment error")?;
            Err(e.context(format!(
                "worker cannot run task {id}; it is back in the queue"
            )))
        }
    }
}

/// The rolling 24-hour cap. `Some(message)` when nothing more may start.
pub fn day_budget_reached(f: &Forge) -> Result<Option<String>> {
    let spent = f.store.spent_since(unix_now() - 86_400)?;
    Ok((spent >= f.budget.per_day_usd).then(|| {
        format!(
            "daily budget reached: ${spent:.2} of ${:.2} in the last 24h (per_day_usd in {})",
            f.budget.per_day_usd,
            f.paths.home.join("config.toml").display()
        )
    }))
}

/// Drain the queue one task at a time, then exit.
pub async fn work(f: Arc<Forge>, max_tasks: Option<u32>) -> Result<()> {
    for id in f.store.orphans(pid_alive)? {
        f.store.requeue(id, "previous worker exited")?;
        eprintln!("requeued task {id}: its previous worker exited");
    }
    let pid = std::process::id() as i64;
    let (mut done, mut ok) = (0u32, 0u32);
    while max_tasks.is_none_or(|m| done < m) {
        if let Some(msg) = day_budget_reached(&f)? {
            eprintln!("{msg}; {} task(s) left queued", f.store.queued_count()?);
            break;
        }
        let Some(t) = f.store.claim_next(pid)? else {
            break;
        };
        eprintln!(
            "======== task {} ({} queued after this)",
            t.id,
            f.store.queued_count()?
        );
        if drive(f.clone(), t.id).await? == TaskState::Succeeded {
            ok += 1;
        }
        done += 1;
        eprintln!();
    }
    eprintln!("worked {done} task(s): {ok} succeeded, {} not", done - ok);
    Ok(())
}
