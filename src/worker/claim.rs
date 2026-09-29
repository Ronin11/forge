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
    if capacity::load_holds(&f.worker, capacity::load_per_core()) {
        return Ok(None);
    }
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
    if capacity::load_holds(&f.worker, capacity::load_per_core()) {
        return Ok(None);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{JobState, Project, TaskState};
    use std::collections::BTreeMap;

    fn enqueue(f: &Forge, project: &str) -> i64 {
        if f.store.project(project).unwrap().is_none() {
            f.store
                .create_project(&Project {
                    name: project.into(),
                    ..Default::default()
                })
                .unwrap();
        }
        let mut t = crate::worker::tests::task_on("code");
        t.project = Some(project.into());
        f.store.insert_task(&t).unwrap()
    }

    fn opts() -> WorkOpts {
        WorkOpts {
            jobs: None,
            poll: None,
            max_tasks: None,
        }
    }

    #[test]
    fn capacity_claim_skips_a_full_project_and_counts_jobs() {
        let (_dir, mut f) = crate::worker::tests::fixture();
        f.worker.slots = 3;
        f.worker.project_slots = Some(1);
        let heavy = enqueue(&f, "heavy");
        let waiting = enqueue(&f, "heavy");
        let light = enqueue(&f, "light");
        let pid = i64::from(std::process::id());
        assert_eq!(
            task(&f, &opts(), pid, &[], |_| false).unwrap().unwrap().id,
            heavy
        );
        assert_eq!(
            task(&f, &opts(), pid, &[], |_| false).unwrap().unwrap().id,
            light
        );
        assert!(task(&f, &opts(), pid, &[], |_| false).unwrap().is_none());
        assert_eq!(
            f.store.task(waiting).unwrap().unwrap().state,
            TaskState::Queued
        );
        let id = f
            .store
            .create_job(&Job {
                project: "heavy".into(),
                state: JobState::Queued,
                ..Default::default()
            })
            .unwrap();
        assert!(job(&f, &opts()).unwrap().is_none());
        f.worker.projects = BTreeMap::from([("heavy".into(), 2)]);
        assert_eq!(job(&f, &opts()).unwrap().unwrap().id, id);
        // All three machine slots are now occupied, by two tasks and a job.
        f.worker.projects.insert("heavy".into(), 3);
        assert!(task(&f, &opts(), pid, &[], |_| false).unwrap().is_none());
    }

    #[test]
    fn capacity_claim_is_atomic_across_competing_workers() {
        let (_dir, f) = crate::worker::tests::fixture();
        enqueue(&f, "heavy");
        enqueue(&f, "light");
        let other = f.reopen().unwrap();
        let gate = std::sync::Barrier::new(2);
        let claim = |f: &Forge| {
            gate.wait();
            usize::from(
                task(f, &opts(), i64::from(std::process::id()), &[], |_| false)
                    .unwrap()
                    .is_some(),
            )
        };
        let counts = std::thread::scope(|scope| {
            let a = scope.spawn(|| claim(&f));
            let b = scope.spawn(|| claim(&other));
            a.join().unwrap() + b.join().unwrap()
        });
        assert_eq!(counts, 1);
        assert_eq!(capacity::used(&f.store).unwrap().values().sum::<usize>(), 1);
    }
}
