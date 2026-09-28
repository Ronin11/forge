//! `forge add --supersedes`: the column recording that a new task
//! replaces an earlier failed or blocked one (`Task::supersedes`), the
//! query that finds the task (if any) that superseded a given one, and
//! the check `forge task set --after` uses to move a blocked task whose
//! re-pointed prerequisites are already queued, running or landed back
//! to `queued` instead of leaving it blocked until retried by hand.

use super::*;

impl Store {
    /// The newest task that supersedes `id`, if any: what `forge show`
    /// prints as "superseded by N" on the task that was replaced.
    pub fn superseded_by(&self, id: i64) -> Result<Option<i64>> {
        Ok(self.lock().retry_query_row(
            "SELECT MAX(id) FROM tasks WHERE supersedes=?1",
            params![id],
            |r| r.get::<_, Option<i64>>(0),
        )?)
    }

    /// A blocked task's `--after` was just replaced with `after`: move it
    /// back to `queued` when none of the new prerequisites are still
    /// blocked, failed, unverified, withdrawn or missing — a dependency
    /// that is queued, running, or succeeded (and landed, unless told not
    /// to) is no reason to stay blocked. Otherwise leaves it blocked, as
    /// before this existed. Returns whether it was reopened.
    pub fn reopen_blocked_if_ready(&self, id: i64, after: &[i64]) -> Result<bool> {
        let c = self.lock();
        for &d in after {
            let row: Option<(String, bool, String)> = c
                .retry_query_row(
                    "SELECT state, land, landed_sha FROM tasks WHERE id=?1",
                    params![d],
                    |r| Ok((r.get("state")?, r.get("land")?, r.get("landed_sha")?)),
                )
                .optional()?;
            let ready = match row {
                Some((state, land, landed_sha)) => {
                    matches!(state.as_str(), "queued" | "running")
                        || (state == "succeeded" && (!land || !landed_sha.is_empty()))
                }
                None => false,
            };
            if !ready {
                return Ok(false);
            }
        }
        let n = c.retry_execute(
            "UPDATE tasks SET state='queued', reason='', finished_at=NULL WHERE id=?1 AND state='blocked'",
            params![id],
        )?;
        Ok(n == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(s: &Store) -> i64 {
        s.insert_task(&Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 2,
            timeout_secs: 1,
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn superseded_by_finds_the_newest_task_naming_this_one_as_superseded() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let old = task(&s);
        assert_eq!(s.superseded_by(old).unwrap(), None);
        let mut newer = Task {
            supersedes: Some(old),
            ..Default::default()
        };
        newer.repo = "r".into();
        newer.task = "t2".into();
        newer.base_branch = "main".into();
        newer.model = "m".into();
        newer.max_turns = 1;
        newer.max_attempts = 2;
        newer.timeout_secs = 1;
        let new_id = s.insert_task(&newer).unwrap();
        assert_eq!(s.superseded_by(old).unwrap(), Some(new_id));
    }

    #[test]
    fn a_supersedes_dependent_is_re_pointed_at_the_new_task() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let old = task(&s);
        let dependent = s
            .insert_task(&Task {
                repo: "r".into(),
                task: "t".into(),
                base_branch: "main".into(),
                model: "m".into(),
                max_turns: 1,
                max_attempts: 1,
                timeout_secs: 1,
                after: vec![old],
                ..Default::default()
            })
            .unwrap();
        let new_id = s
            .insert_task(&Task {
                repo: "r".into(),
                task: "t2".into(),
                base_branch: "main".into(),
                model: "m".into(),
                max_turns: 1,
                max_attempts: 1,
                timeout_secs: 1,
                supersedes: Some(old),
                ..Default::default()
            })
            .unwrap();

        let moved = s.reroute_dependents(old, new_id).unwrap();

        assert_eq!(moved, vec![dependent]);
        assert_eq!(s.task(dependent).unwrap().unwrap().after, vec![new_id]);
    }

    #[test]
    fn reopen_blocked_if_ready_requeues_only_when_every_new_dependency_is_past_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let dep_running = task(&s);
        assert!(s.claim(dep_running, 1).unwrap());
        let dep_failed = task(&s);
        s.update_task(&Task {
            id: dep_failed,
            state: TaskState::Failed,
            ..s.task(dep_failed).unwrap().unwrap()
        })
        .unwrap();
        let blocked = task(&s);
        s.update_task(&Task {
            id: blocked,
            state: TaskState::Blocked,
            reason: "waits on task 1".into(),
            ..s.task(blocked).unwrap().unwrap()
        })
        .unwrap();

        assert!(
            !s.reopen_blocked_if_ready(blocked, &[dep_running, dep_failed])
                .unwrap(),
            "a failed dependency must not reopen the task"
        );
        assert_eq!(s.task(blocked).unwrap().unwrap().state, TaskState::Blocked);

        assert!(
            s.reopen_blocked_if_ready(blocked, &[dep_running]).unwrap(),
            "a running dependency is no reason to stay blocked"
        );
        let reopened = s.task(blocked).unwrap().unwrap();
        assert_eq!(reopened.state, TaskState::Queued);
        assert_eq!(reopened.reason, "");
    }
}
