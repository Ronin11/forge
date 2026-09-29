//! `forge audit`'s window: which lineages had activity since a time, fed
//! into `crate::lineage`'s pure classification alongside the already tested
//! `Store::lineage` and `Store::root_of`.

use super::*;
use std::collections::BTreeSet;

/// One task in a lineage: `parent` is what it retries, `supersedes` the
/// earlier task it replaces (`forge add --supersedes`); both are follow-up
/// links `crate::lineage`'s tip walk treats the same way.
#[derive(Debug, Clone)]
pub struct LineageRow {
    pub id: i64,
    pub parent: Option<i64>,
    pub supersedes: Option<i64>,
    pub state: String,
    pub reason: String,
    pub workflow: String,
    pub cost: f64,
    /// Land on the base branch once verified (see `Task::land`); with
    /// `landed_sha`, tells a succeeded row apart from one still pending a
    /// human's `forge land` (see `crate::lineage::classify`).
    pub land: bool,
    pub landed_sha: String,
}

impl Store {
    /// The first task in `id`'s chain of retries and supersessions: itself
    /// when it neither retries nor supersedes anything.
    pub fn root_of(&self, id: i64) -> Result<i64> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "WITH RECURSIVE up(id, prior) AS (
               SELECT id, COALESCE(retry_of, supersedes) FROM tasks WHERE id = ?1
               UNION ALL
               SELECT t.id, COALESCE(t.retry_of, t.supersedes)
               FROM up JOIN tasks t ON t.id = up.prior)
             SELECT id FROM up",
        )?;
        let ids: Vec<i64> = stmt
            .query_map(params![id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // Follow-up links always point at an already-existing task, so ids
        // only shrink walking up the chain: the root is the smallest one.
        Ok(ids.into_iter().min().unwrap())
    }

    /// Every task in `id`'s lineage, root first: the root and everything
    /// that retries or supersedes it, directly or through other follow-ups.
    pub fn lineage(&self, id: i64) -> Result<Vec<LineageRow>> {
        let root = self.root_of(id)?;
        let c = self.lock();
        let mut stmt = c.prepare(
            "WITH RECURSIVE down(id) AS (
               SELECT ?1 UNION ALL
               SELECT t.id FROM down JOIN tasks t
                 ON t.retry_of = down.id OR t.supersedes = down.id)
             SELECT t.id, t.retry_of, t.supersedes, t.state, t.reason, t.workflow,
                    t.land, t.landed_sha,
                    COALESCE((SELECT SUM(cost_usd) FROM attempts a WHERE a.task_id = t.id), 0) AS cost
             FROM down JOIN tasks t ON t.id = down.id ORDER BY t.id",
        )?;
        let rows = stmt.query_map(params![root], |r| {
            Ok(LineageRow {
                id: r.get("id")?,
                parent: r.get("retry_of")?,
                supersedes: r.get("supersedes")?,
                state: r.get("state")?,
                reason: r.get("reason")?,
                workflow: r.get("workflow")?,
                cost: r.get("cost")?,
                land: r.get::<_, i64>("land")? != 0,
                landed_sha: r.get("landed_sha")?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

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

    #[test]
    fn root_of_and_lineage_follow_a_supersede_link_like_a_retry() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let old = queued(&s);
        let mut newer = s.task(queued(&s)).unwrap().unwrap();
        newer.supersedes = Some(old);
        s.update_task(&newer).unwrap();

        assert_eq!(s.root_of(newer.id).unwrap(), old);
        let rows = s.lineage(old).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            [old, newer.id]
        );
        assert_eq!(rows[1].supersedes, Some(old));
    }
}
