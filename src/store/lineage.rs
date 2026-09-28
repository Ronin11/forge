//! `forge audit`'s window: which lineages had activity since a time, fed
//! into `crate::lineage`'s pure classification alongside the already tested
//! `Store::lineage` and `Store::root_of`.

use super::*;
use std::collections::BTreeSet;

impl Store {
    /// Every task id created at or after `since` (unix seconds), any state.
    pub fn task_ids_since(&self, since: i64) -> Result<Vec<i64>> {
        let c = self.lock();
        let mut stmt = c.prepare("SELECT id FROM tasks WHERE created_at >= ?1 ORDER BY id")?;
        let rows = stmt.query_map(params![since], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The distinct lineage roots among tasks created since `since`: every
    /// task touched in the window, walked up to its root (`root_of`),
    /// deduplicated. A lineage's root itself may predate `since` when only
    /// a later retry in its chain falls in the window.
    pub fn roots_since(&self, since: i64) -> Result<Vec<i64>> {
        let mut roots = BTreeSet::new();
        for id in self.task_ids_since(since)? {
            roots.insert(self.root_of(id)?);
        }
        Ok(roots.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Task;

    fn queued(s: &Store) -> i64 {
        s.insert_task(&Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 2,
            timeout_secs: 1,
            created_at: crate::unix_now(),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn task_ids_since_excludes_tasks_before_the_cutoff() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let id = queued(&s);
        let now = crate::unix_now();
        assert_eq!(s.task_ids_since(now - 60).unwrap(), vec![id]);
        assert_eq!(s.task_ids_since(now + 60 * 60).unwrap(), Vec::<i64>::new());
    }

    #[test]
    fn roots_since_dedupes_a_retry_to_its_root() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let root = queued(&s);
        let mut retry = s.task(queued(&s)).unwrap().unwrap();
        retry.retry_of = Some(root);
        s.update_task(&retry).unwrap();
        let now = crate::unix_now();
        assert_eq!(s.roots_since(now - 60).unwrap(), vec![root]);
        assert_eq!(s.roots_since(now + 60 * 60).unwrap(), Vec::<i64>::new());
    }
}
