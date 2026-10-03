//! `forge add --supersedes`: at enqueue time, the target must exist, and
//! whatever waited on it is re-pointed to the new task the same way a
//! retry's dependents are (`Store::reroute_dependents` does not care
//! which of the two made the new task).

use super::*;

/// What `--supersedes OLD` must hold: the task exists. Unlike `--after`,
/// a superseded task need not land — the new task replaces its work
/// rather than building on it.
pub(super) fn supersede_fits(store: &crate::store::Store, old: i64) -> Result<()> {
    if store.task(old)?.is_none() {
        bail!("--supersedes {old}: no such task");
    }
    Ok(())
}

/// After `new` is inserted: a retry's or a supersede's dependents (tasks
/// whose `after` named `old`) are re-pointed at `new`, and a dependent
/// swept into blocked when `old` ended is queued again. `why` reads into
/// "waits on task N now (<why> task OLD)", e.g. "a retry of" or "a
/// supersede of".
pub(super) fn reroute(f: &Forge, old: i64, new: &Task, why: &str) -> Result<()> {
    for d in f.store.reroute_dependents(old, new.id)? {
        f.report.emit(
            d,
            Event::Note {
                text: &format!("waits on task {} now ({why} task {old})", new.id),
            },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn supersede_fits_refuses_a_task_that_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let err = supersede_fits(&s, 999).unwrap_err();
        assert!(err.to_string().contains("no such task"), "{err}");
    }

    #[test]
    fn supersede_fits_accepts_a_task_that_exists() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let id = s
            .insert_task(&Task {
                repo: "r".into(),
                task: "t".into(),
                base_branch: "main".into(),
                model: "m".into(),
                max_turns: 1,
                max_attempts: 1,
                timeout_secs: 1,
                ..Default::default()
            })
            .unwrap();
        assert!(supersede_fits(&s, id).is_ok());
    }
}
