//! Schedule refusals: the schedules the worker's tick is holding back
//! because a `per_day` cap refuses their start. The tick logs a refusal
//! once, when it begins, and once when it clears; this table is where the
//! state in between is queried (`forge job list`).

use super::*;

/// One schedule whose start is refused, since when, and until when.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScheduleRefusal {
    pub project: String,
    pub workflow: String,
    /// Unix second the tick first saw the refusal.
    pub since: i64,
    /// Unix second the cap next allows a start, when known.
    pub next_allowed: Option<i64>,
    /// The refusal's text.
    pub reason: String,
}

impl Store {
    /// Record that `refusal.project`'s `refusal.workflow` is refused,
    /// replacing what was recorded before.
    pub fn set_schedule_refusal(&self, refusal: &ScheduleRefusal) -> Result<()> {
        self.lock().retry_execute(
            "INSERT INTO schedule_refusals (project, workflow, since, next_allowed, reason)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project, workflow) DO UPDATE SET since=?3, next_allowed=?4, reason=?5",
            params![
                refusal.project,
                refusal.workflow,
                refusal.since,
                refusal.next_allowed,
                refusal.reason
            ],
        )?;
        Ok(())
    }

    /// The refusal ended (or the schedule is gone). Returns the row it
    /// removed, if there was one.
    pub fn clear_schedule_refusal(
        &self,
        project: &str,
        workflow: &str,
    ) -> Result<Option<ScheduleRefusal>> {
        let c = self.lock();
        let row = c
            .retry_query_row(
                "SELECT project, workflow, since, next_allowed, reason FROM schedule_refusals WHERE project=?1 AND workflow=?2",
                params![project, workflow],
                refusal_from_row,
            )
            .optional()?;
        c.retry_execute(
            "DELETE FROM schedule_refusals WHERE project=?1 AND workflow=?2",
            params![project, workflow],
        )?;
        Ok(row)
    }

    /// Every schedule now refused, by project and workflow.
    pub fn schedule_refusals(&self) -> Result<Vec<ScheduleRefusal>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT project, workflow, since, next_allowed, reason FROM schedule_refusals ORDER BY project, workflow",
        )?;
        let rows = stmt.query_map([], refusal_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn refusal_from_row(r: &rusqlite::Row) -> rusqlite::Result<ScheduleRefusal> {
    Ok(ScheduleRefusal {
        project: r.get("project")?,
        workflow: r.get("workflow")?,
        since: r.get("since")?,
        next_allowed: r.get("next_allowed")?,
        reason: r.get("reason")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_recorded_replaced_listed_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("forge.db")).unwrap();
        let mut r = ScheduleRefusal {
            project: "forge".into(),
            workflow: "gc-nightly".into(),
            since: 100,
            next_allowed: Some(500),
            reason: "cap".into(),
        };
        store.set_schedule_refusal(&r).unwrap();
        r.next_allowed = Some(600);
        store.set_schedule_refusal(&r).unwrap();
        assert_eq!(store.schedule_refusals().unwrap(), vec![r.clone()]);
        assert_eq!(
            store.clear_schedule_refusal("forge", "gc-nightly").unwrap(),
            Some(r)
        );
        assert!(store.schedule_refusals().unwrap().is_empty());
        assert_eq!(
            store.clear_schedule_refusal("forge", "gc-nightly").unwrap(),
            None
        );
    }
}
