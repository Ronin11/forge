//! The event trigger's place in the log (docs/JOBS.md, "Triggers"): for
//! each project's run workflow with `[trigger] on = "event"`, the byte
//! offset in `events.jsonl` up to which the worker has examined events. A
//! row per workflow, so adding a second event-triggered workflow later
//! starts it at the end of the log rather than replaying history, and a
//! restart resumes where the last tick stopped.

use super::*;

#[cfg(test)]
pub(super) const EVENT_CURSOR_COLUMNS: &[&str] = &["project", "workflow", "event_offset"];

impl Store {
    /// The offset `project`'s `workflow` has examined events up to, or
    /// `None` if the event tick has never seen it.
    pub fn event_cursor(&self, project: &str, workflow: &str) -> Result<Option<i64>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT event_offset FROM event_cursors WHERE project=?1 AND workflow=?2",
                params![project, workflow],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Record that `project`'s `workflow` has examined events up to
    /// `offset`. Not monotonic: the worker sets it back to 0 when the log
    /// has rolled to a shorter file.
    pub fn set_event_cursor(&self, project: &str, workflow: &str, offset: i64) -> Result<()> {
        self.lock().execute(
            "INSERT INTO event_cursors (project, workflow, event_offset) VALUES (?1, ?2, ?3)
             ON CONFLICT(project, workflow) DO UPDATE SET event_offset=excluded.event_offset",
            params![project, workflow, offset],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_is_per_project_and_workflow_and_moves_either_way() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("forge.db")).unwrap();
        s.lock()
            .execute(
                "INSERT INTO projects (name, purpose, created_at) VALUES ('shop', 'p', 1)",
                [],
            )
            .unwrap();
        assert_eq!(s.event_cursor("shop", "on-done").unwrap(), None);
        s.set_event_cursor("shop", "on-done", 120).unwrap();
        s.set_event_cursor("shop", "on-deploy", 7).unwrap();
        assert_eq!(s.event_cursor("shop", "on-done").unwrap(), Some(120));
        assert_eq!(s.event_cursor("shop", "on-deploy").unwrap(), Some(7));
        s.set_event_cursor("shop", "on-done", 0).unwrap();
        assert_eq!(s.event_cursor("shop", "on-done").unwrap(), Some(0));
    }
}
