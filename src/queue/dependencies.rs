//! What `--after` must hold, at `enqueue` and at `edit_task`: the task
//! exists, will land, and does not close a cycle back on the task naming
//! it.

use super::*;

/// What `--after DEP` must hold, at enqueue and at edit: the task exists
/// and will land. A dependency means only "wait for that task to reach a
/// terminal state; block if it failed" (see `Store::queued_unblocked` and
/// `Store::block_dependents`, both keyed on the dependency's id and state
/// alone), so it carries across repositories: a task on one repository
/// may wait on a task in another.
pub(super) async fn dependency_fits(f: &Forge, dep: i64) -> Result<()> {
    let Some(d) = f.store.task(dep)? else {
        bail!("--after {dep}: no such task");
    };
    if !d.land && d.state != TaskState::Succeeded {
        bail!(
            "--after {dep}: that task will not land (--no-land), so nothing built on it could see its work"
        );
    }
    // A repository with no push remote never lands anything either
    // (engine::land skips it, leaving `landed_sha` empty), so a
    // dependent would wait on landing that never comes.
    let cfg = config::load_working(std::path::Path::new(&d.repo)).await?;
    if cfg.push_remote.is_none() {
        bail!(
            "--after {dep}: {} has no push remote, so nothing built on it could see its work",
            d.repo
        );
    }
    Ok(())
}

/// Whether `dep` waits, directly or through other tasks, on `id`: the
/// cycle `--after` must not close.
pub(super) fn waits_on(f: &Forge, dep: i64, id: i64) -> Result<bool> {
    let mut seen = std::collections::BTreeSet::new();
    let mut todo = vec![dep];
    while let Some(next) = todo.pop() {
        if next == id {
            return Ok(true);
        }
        if !seen.insert(next) {
            continue;
        }
        if let Some(t) = f.store.task(next)? {
            todo.extend(t.after);
        }
    }
    Ok(false)
}
