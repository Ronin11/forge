//! The task's stored run cursor (`tasks.run_json`, see `engine::cursor`).

use super::*;

impl Store {
    /// The task's run cursor as stored JSON; `None` when it has none.
    pub fn run_cursor(&self, id: i64) -> Result<Option<String>> {
        let raw: Option<String> = self
            .lock()
            .retry_query_row("SELECT run_json FROM tasks WHERE id=?1", params![id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(raw.filter(|s| !s.is_empty()))
    }

    /// Record the task's run cursor; an empty string clears it.
    pub fn set_run_cursor(&self, id: i64, cursor: &str) -> Result<()> {
        self.lock().retry_execute(
            "UPDATE tasks SET run_json=?2 WHERE id=?1",
            params![id, cursor],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Attempt, AttemptState, Caller, Task};

    fn running_task_with_attempt(s: &Store) -> (i64, i64) {
        let id = s
            .insert_task(&Task {
                repo: "r".into(),
                task: "t".into(),
                base_branch: "main".into(),
                model: "m".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(s.claim(id, 1).unwrap());
        let a = s
            .insert_attempt(&Attempt {
                task_id: id,
                attempt_no: 1,
                ..Default::default()
            })
            .unwrap();
        (id, a)
    }

    #[test]
    fn an_attempt_and_the_run_cursor_finish_in_one_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let (id, a) = running_task_with_attempt(&s);
        let fin = crate::store::FinishAttempt {
            id: a,
            state: AttemptState::Succeeded,
            ..Default::default()
        };
        s.lock()
            .execute_batch(
                "CREATE TRIGGER no_cursor BEFORE UPDATE OF run_json ON tasks
                 BEGIN SELECT RAISE(ABORT, 'cursor rejected'); END;",
            )
            .unwrap();
        assert!(s.finish_attempt_with_cursor(&fin, "{}").is_err());
        assert_eq!(
            s.attempts(id).unwrap()[0].state,
            AttemptState::Running,
            "a rejected cursor leaves the attempt unfinished, not succeeded"
        );
        s.lock().execute_batch("DROP TRIGGER no_cursor").unwrap();
        s.finish_attempt_with_cursor(&fin, "{\"idx\":2}").unwrap();
        assert_eq!(s.attempts(id).unwrap()[0].state, AttemptState::Succeeded);
        assert_eq!(s.run_cursor(id).unwrap().as_deref(), Some("{\"idx\":2}"));
    }

    #[test]
    fn a_requeue_records_the_cursor_with_the_closed_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let (id, _) = running_task_with_attempt(&s);
        let owner = s.orphans(&Caller::this_process(false), |_| false).unwrap()[0]
            .1
            .clone();
        assert!(
            s.requeue_at(id, &owner, "window", Some("{\"idx\":2}"))
                .unwrap()
        );
        assert_eq!(s.run_cursor(id).unwrap().as_deref(), Some("{\"idx\":2}"));
        assert_eq!(s.attempts(id).unwrap()[0].state, AttemptState::AgentFailed);
    }
}
