//! The event trigger's place in the log (docs/JOBS.md, "Triggers"): for
//! each project's run workflow with `[trigger] on = "event"`, the generation and byte
//! offset in `events.jsonl` up to which the worker has examined events. A
//! row per workflow, so adding a second event-triggered workflow later
//! starts it at the end of the log rather than replaying history, and a
//! restart resumes where the last tick stopped.

use super::*;

#[cfg(test)]
pub(super) const EVENT_CURSOR_COLUMNS: &[&str] = &["project", "workflow", "event_offset", "cursor"];

impl Store {
    /// The offset `project`'s `workflow` has examined events up to, or
    /// `None` if the event tick has never seen it.
    pub fn event_cursor(&self, project: &str, workflow: &str) -> Result<Option<String>> {
        Ok(self
            .lock()
            .retry_query_row(
                "SELECT cursor FROM event_cursors WHERE project=?1 AND workflow=?2",
                params![project, workflow],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Record that `project`'s `workflow` has examined events up to
    /// `generation:offset`, including progress across rotations.
    pub fn set_event_cursor(&self, project: &str, workflow: &str, offset: &str) -> Result<()> {
        self.lock().retry_execute(
            "INSERT INTO event_cursors (project, workflow, event_offset, cursor) VALUES (?1, ?2, 0, ?3)
             ON CONFLICT(project, workflow) DO UPDATE SET cursor=excluded.cursor",
            params![project, workflow, offset],
        )?;
        Ok(())
    }
}

impl Store {
    /// The `generation:offset` cursor after the last line delivered to
    /// the subscriber `name`, or `None` if it has never received one.
    pub fn subscription_cursor(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .lock()
            .retry_query_row(
                "SELECT cursor FROM subscriptions WHERE name=?1",
                params![name],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Record that `name` has been delivered everything before `cursor`.
    pub fn set_subscription_cursor(&self, name: &str, cursor: &str) -> Result<()> {
        self.lock().retry_execute(
            "INSERT INTO subscriptions (name, cursor, updated_at) VALUES (?1, ?2, strftime('%s','now'))
             ON CONFLICT(name) DO UPDATE SET cursor=excluded.cursor, updated_at=excluded.updated_at",
            params![name, cursor],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_cursor_migration_preserves_legacy_positions_and_job_keys() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE event_cursors (event_offset INTEGER);
            INSERT INTO event_cursors VALUES (123);
            CREATE TABLE jobs (trigger_kind TEXT, trigger_ref TEXT);
            INSERT INTO jobs VALUES ('event', '17'), ('webhook', '17');",
        )
        .unwrap();
        conn.execute_batch(
            super::super::migrations::MIGRATIONS
                .iter()
                .find(|sql| sql.contains("ADD COLUMN cursor TEXT"))
                .unwrap(),
        )
        .unwrap();
        let cursor: String = conn
            .query_row("SELECT cursor FROM event_cursors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cursor, "0:123");
        let key: String = conn
            .query_row(
                "SELECT trigger_ref FROM jobs WHERE trigger_kind='event'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(key, "0:17");
        let key: String = conn
            .query_row(
                "SELECT trigger_ref FROM jobs WHERE trigger_kind='webhook'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(key, "17");
    }

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
        s.set_event_cursor("shop", "on-done", "0:120").unwrap();
        s.set_event_cursor("shop", "on-deploy", "0:7").unwrap();
        assert_eq!(
            s.event_cursor("shop", "on-done").unwrap(),
            Some("0:120".into())
        );
        assert_eq!(
            s.event_cursor("shop", "on-deploy").unwrap(),
            Some("0:7".into())
        );
        s.set_event_cursor("shop", "on-done", "0:0").unwrap();
        assert_eq!(
            s.event_cursor("shop", "on-done").unwrap(),
            Some("0:0".into())
        );
    }

    #[test]
    fn a_subscription_cursor_is_kept_per_name() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("forge.db")).unwrap();
        assert_eq!(s.subscription_cursor("notify").unwrap(), None);
        s.set_subscription_cursor("notify", "0:120").unwrap();
        s.set_subscription_cursor("other", "1:7").unwrap();
        s.set_subscription_cursor("notify", "0:200").unwrap();
        assert_eq!(
            s.subscription_cursor("notify").unwrap().as_deref(),
            Some("0:200")
        );
        assert_eq!(
            s.subscription_cursor("other").unwrap().as_deref(),
            Some("1:7")
        );
    }
}
