//! Capacity and claim updates share a SQLite write transaction. Two workers
//! racing (including a successor and its predecessor) cannot spend one slot twice.
use super::{WorkOpts, capacity};
use crate::{
    ctx::Forge,
    store::{Job, Task},
};
use anyhow::Result;
use rusqlite::{TransactionBehavior, params};

pub fn task(
    f: &Forge,
    opts: &WorkOpts,
    pid: i64,
    held: &[i64],
    blocked: impl Fn(&Task) -> bool,
) -> Result<Option<Task>> {
    for t in f.store.queued_unblocked(held)? {
        if blocked(&t) {
            continue;
        }
        let claimed = {
            let mut conn = f.store.lock();
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let used = capacity::usage(&tx)?;
            if !f.worker.allows(
                capacity::slots(f, opts),
                &used,
                t.project.as_deref().unwrap_or(&t.repo),
            ) {
                continue;
            }
            let changed = tx.execute(
                "UPDATE tasks SET state='running', worker_pid=?2, worker_start=?3, started_at=?4 WHERE id=?1 AND state='queued'",
                params![t.id, pid, crate::store::start_of(pid), crate::unix_now()],
            )?;
            tx.commit()?;
            changed == 1
        };
        if claimed {
            return f.store.task(t.id);
        }
    }
    Ok(None)
}

pub fn job(f: &Forge, opts: &WorkOpts) -> Result<Option<Job>> {
    let id = {
        let mut conn = f.store.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let used = capacity::usage(&tx)?;
        let candidates = {
            let mut stmt = tx.prepare("SELECT id, project FROM jobs WHERE state='queued' OR (state='scheduled' AND due_at <= ?1) ORDER BY id")?;
            stmt.query_map([crate::unix_now()], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let id = candidates
            .into_iter()
            .find(|(_, project)| f.worker.allows(capacity::slots(f, opts), &used, project))
            .map(|(id, _)| id);
        if let Some(id) = id {
            let pid = i64::from(std::process::id());
            tx.execute(
                "UPDATE jobs SET state='running', worker_pid=?2, worker_start=?3 WHERE id=?1",
                params![id, pid, crate::store::start_of(pid)],
            )?;
        }
        tx.commit()?;
        id
    };
    match id {
        Some(id) => f.store.job(id),
        None => Ok(None),
    }
}
