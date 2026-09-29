//! The workers table (docs/OPS.md, "The running binary"): every daemon
//! worker's pid and the release it runs, in the order they registered. Two
//! versions share the store while the old one drains; this is how each
//! knows which one claims.

use super::*;

/// One registered worker. `id` orders registrations: a larger id is newer.
#[derive(Debug, Clone)]
pub struct WorkerRow {
    pub id: i64,
    pub pid: i64,
    pub version: String,
    /// The slot count it runs with; 0 when it never recorded one.
    pub slots: usize,
    /// The registering process's start identity (`start_of`); empty for a
    /// row written before this was recorded, or where `/proc` gave none.
    pub start: String,
}

impl Store {
    /// Record a worker of release `version` under `pid`, returning its id.
    /// A live row already recorded for this pid and version (the one a
    /// parent wrote when it started this process) is reused; anything else
    /// under the pid is a dead process's and is closed. `start_of(pid)`
    /// rides along, so a pid later reused by another process is not
    /// mistaken for this worker (`live_workers`).
    pub fn register_worker(&self, pid: i64, version: &str) -> Result<i64> {
        let c = self.lock();
        let open: Option<i64> = c
            .retry_query_row(
                "SELECT id FROM workers WHERE pid=?1 AND version=?2 AND stopped_at IS NULL",
                params![pid, version],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = open {
            return Ok(id);
        }
        let now = crate::unix_now();
        c.retry_execute(
            "UPDATE workers SET stopped_at=?2 WHERE pid=?1 AND stopped_at IS NULL",
            params![pid, now],
        )?;
        c.retry_execute(
            "INSERT INTO workers (pid, version, started_at, start) VALUES (?1, ?2, ?3, ?4)",
            params![pid, version, now, super::start_of(pid).unwrap_or_default()],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Record how many slots worker `id` runs with.
    pub fn set_worker_slots(&self, id: i64, slots: usize) -> Result<()> {
        self.lock().retry_execute(
            "UPDATE workers SET slots=?2 WHERE id=?1",
            params![id, slots as i64],
        )?;
        Ok(())
    }

    /// The worker exited: it no longer counts, whatever its pid does.
    pub fn stop_worker(&self, id: i64) -> Result<()> {
        self.lock().retry_execute(
            "UPDATE workers SET stopped_at=?2 WHERE id=?1 AND stopped_at IS NULL",
            params![id, crate::unix_now()],
        )?;
        Ok(())
    }

    /// Workers not stopped whose process still is the one that registered:
    /// `alive` gets each row's pid and its recorded start identity, the way
    /// orphan recovery checks a claimed row's owner (`Caller::is_orphan`).
    pub fn live_workers(&self, alive: impl Fn(i64, &str) -> bool) -> Result<Vec<WorkerRow>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT id, pid, version, slots, start FROM workers WHERE stopped_at IS NULL ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(WorkerRow {
                    id: r.get("id")?,
                    pid: r.get("pid")?,
                    version: r.get("version")?,
                    slots: r.get::<_, i64>("slots")?.max(0) as usize,
                    start: r.get("start")?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .filter(|w| alive(w.pid, &w.start))
            .collect())
    }

    /// Tasks and jobs running under every live registered worker but `pid`:
    /// the slots of the machine's budget that are not this worker's to use.
    pub fn running_elsewhere(&self, pid: i64, alive: impl Fn(i64, &str) -> bool) -> Result<usize> {
        let mut n = 0;
        for w in self.live_workers(alive)? {
            if w.pid != pid {
                n += self.held_by_worker(w.pid)?.max(0) as usize;
            }
        }
        Ok(n)
    }

    /// Tasks and jobs `pid` is running right now.
    pub fn held_by_worker(&self, pid: i64) -> Result<i64> {
        let c = self.lock();
        let n: i64 = c.retry_query_row(
            "SELECT (SELECT COUNT(*) FROM tasks WHERE state='running' AND worker_pid=?1)
                  + (SELECT COUNT(*) FROM jobs WHERE state='running' AND worker_pid=?1)",
            [pid],
            |r| r.get(0),
        )?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_orders_workers_and_a_stopped_one_no_longer_counts() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let a = s.register_worker(10, "old").unwrap();
        let b = s.register_worker(11, "new").unwrap();
        assert_eq!(s.register_worker(11, "new").unwrap(), b);
        assert!(b > a);
        let live = s.live_workers(|_, _| true).unwrap();
        assert_eq!(live.iter().map(|w| w.pid).collect::<Vec<_>>(), [10, 11]);
        assert_eq!(s.live_workers(|p, _| p == 11).unwrap().len(), 1);
        s.stop_worker(a).unwrap();
        assert_eq!(s.live_workers(|_, _| true).unwrap().len(), 1);
    }

    #[test]
    fn what_the_other_workers_run_is_counted_and_a_workers_own_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let old = s.register_worker(10, "old").unwrap();
        let new = s.register_worker(11, "new").unwrap();
        s.set_worker_slots(old, 4).unwrap();
        s.set_worker_slots(new, 4).unwrap();
        let live = s.live_workers(|_, _| true).unwrap();
        assert_eq!(live.iter().map(|w| w.slots).collect::<Vec<_>>(), [4, 4]);
        assert_eq!(s.running_elsewhere(11, |_, _| true).unwrap(), 0);
        for _ in 0..2 {
            s.insert_task(&Task {
                repo: "r".into(),
                task: "t".into(),
                base_branch: "main".into(),
                ..Default::default()
            })
            .unwrap();
            s.claim_next(10, &[], |_| false).unwrap().unwrap();
        }
        assert_eq!(s.running_elsewhere(11, |_, _| true).unwrap(), 2);
        assert_eq!(s.running_elsewhere(10, |_, _| true).unwrap(), 0);
        assert_eq!(s.running_elsewhere(11, |p, _| p == 11).unwrap(), 0);
    }

    /// REVIEW-4 E2-2: `register_worker` records the start identity next to
    /// the row, and `live_workers` hands it to the caller's check the way
    /// `Caller::is_orphan` compares a claimed row's owner: a caller that
    /// treats a different current start as a reused pid sees no row for
    /// it, and one that treats the recorded start as still current does.
    #[test]
    fn the_recorded_start_rides_along_and_a_mismatch_is_the_callers_to_judge() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let id = s.register_worker(10, "old").unwrap();
        s.lock()
            .execute("UPDATE workers SET start='1000' WHERE id=?1", [id])
            .unwrap();
        assert_eq!(s.live_workers(|_, _| true).unwrap()[0].start, "1000");
        assert!(
            s.live_workers(|pid, start| pid == 10 && start == "2000")
                .unwrap()
                .is_empty(),
            "pid 10 is now a different process, started at 2000"
        );
        assert_eq!(
            s.live_workers(|pid, start| pid == 10 && start == "1000")
                .unwrap()
                .len(),
            1
        );
    }
}
