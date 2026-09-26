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
}

impl Store {
    /// Record a worker of release `version` under `pid`, returning its id.
    /// A live row already recorded for this pid and version (the one a
    /// parent wrote when it started this process) is reused; anything else
    /// under the pid is a dead process's and is closed.
    pub fn register_worker(&self, pid: i64, version: &str) -> Result<i64> {
        let c = self.lock();
        let open: Option<i64> = c
            .query_row(
                "SELECT id FROM workers WHERE pid=?1 AND version=?2 AND stopped_at IS NULL",
                params![pid, version],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = open {
            return Ok(id);
        }
        let now = crate::unix_now();
        c.execute(
            "UPDATE workers SET stopped_at=?2 WHERE pid=?1 AND stopped_at IS NULL",
            params![pid, now],
        )?;
        c.execute(
            "INSERT INTO workers (pid, version, started_at) VALUES (?1, ?2, ?3)",
            params![pid, version, now],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// The worker exited: it no longer counts, whatever its pid does.
    pub fn stop_worker(&self, id: i64) -> Result<()> {
        self.lock().execute(
            "UPDATE workers SET stopped_at=?2 WHERE id=?1 AND stopped_at IS NULL",
            params![id, crate::unix_now()],
        )?;
        Ok(())
    }

    /// Workers not stopped whose process exists, oldest registration first.
    pub fn live_workers(&self, alive: impl Fn(i64) -> bool) -> Result<Vec<WorkerRow>> {
        let c = self.lock();
        let mut stmt =
            c.prepare("SELECT id, pid, version FROM workers WHERE stopped_at IS NULL ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(WorkerRow {
                    id: r.get(0)?,
                    pid: r.get(1)?,
                    version: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().filter(|w| alive(w.pid)).collect())
    }

    /// Tasks and jobs `pid` is running right now.
    pub fn held_by_worker(&self, pid: i64) -> Result<i64> {
        let c = self.lock();
        let n: i64 = c.query_row(
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
        let live = s.live_workers(|_| true).unwrap();
        assert_eq!(live.iter().map(|w| w.pid).collect::<Vec<_>>(), [10, 11]);
        assert_eq!(s.live_workers(|p| p == 11).unwrap().len(), 1);
        s.stop_worker(a).unwrap();
        assert_eq!(s.live_workers(|_| true).unwrap().len(), 1);
    }
}
