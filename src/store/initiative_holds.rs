//! Which initiatives currently have an announced hold, and with what
//! reason (see `src/worker/holds.rs`): recorded here, not in a worker
//! process's own memory, so a successor started by a self-deploy reads
//! what a predecessor already announced instead of repeating it.

use super::*;

impl Store {
    /// The reason last announced for `id`'s hold, if it is still active (a
    /// row exists only while the hold has not cleared since).
    pub(crate) fn announced_hold_reason(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .lock()
            .retry_query_row(
                "SELECT reason FROM initiative_holds WHERE initiative_id=?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Record `id`'s hold as announced with `reason`, replacing any earlier
    /// reason recorded for it.
    pub(crate) fn record_hold_announced(&self, id: i64, reason: &str, now: i64) -> Result<()> {
        self.lock().retry_execute(
            "INSERT INTO initiative_holds (initiative_id, reason, announced_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(initiative_id) DO UPDATE SET reason=excluded.reason, announced_at=excluded.announced_at",
            params![id, reason, now],
        )?;
        Ok(())
    }

    /// `id`'s hold left `held`: drop its record, so it is announced again
    /// as new the next time it holds, even with the same reason.
    pub(crate) fn clear_announced_hold(&self, id: i64) -> Result<()> {
        self.lock().retry_execute(
            "DELETE FROM initiative_holds WHERE initiative_id=?1",
            params![id],
        )?;
        Ok(())
    }

    /// Every initiative id with a currently active announced hold.
    pub(crate) fn announced_holds(&self) -> Result<Vec<i64>> {
        let c = self.lock();
        let mut stmt = c.prepare("SELECT initiative_id FROM initiative_holds")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hold_is_recorded_reannounced_on_a_changed_reason_and_cleared_on_release() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        assert_eq!(s.announced_hold_reason(1).unwrap(), None);
        s.record_hold_announced(1, "budget: $57.92 of $50.00", 100)
            .unwrap();
        assert_eq!(
            s.announced_hold_reason(1).unwrap().as_deref(),
            Some("budget: $57.92 of $50.00")
        );
        assert_eq!(s.announced_holds().unwrap(), vec![1]);

        // A changed reason on the same initiative overwrites the record.
        s.record_hold_announced(1, "budget: $60.00 of $50.00", 200)
            .unwrap();
        assert_eq!(
            s.announced_hold_reason(1).unwrap().as_deref(),
            Some("budget: $60.00 of $50.00")
        );

        // The hold clears: the record is gone, so a later hold is new.
        s.clear_announced_hold(1).unwrap();
        assert_eq!(s.announced_hold_reason(1).unwrap(), None);
        assert!(s.announced_holds().unwrap().is_empty());
    }
}
