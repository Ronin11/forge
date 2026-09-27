//! Draw a newly inserted task's journal and explore arms inside the same
//! transaction as its insert (see docs/REVIEW-3.md, "Enqueueing writes
//! the whole row a second time"): the row is claimable — or, for a task
//! the concierge already knows to block, visible at all — only once its
//! arms are on it, so nothing ever observes a `journal_arm` still empty.

use super::*;

/// Write `id`'s drawn arms, guarded on the two states a freshly inserted
/// task can be in: `queued` (the ordinary case) and `blocked` (the
/// concierge's "unclear" placeholder and the escalator's proposal, both
/// inserted already blocked — see `queue::TaskRequest::blocked`). Any
/// other state means the row was not the one just inserted in this
/// transaction, so this never matches it. Returns whether it matched a
/// row.
pub(super) fn write_arms(
    conn: &Connection,
    id: i64,
    journal: bool,
    arm: &str,
    explore: &BTreeMap<String, String>,
) -> Result<bool> {
    let n = conn.retry_execute(
        "UPDATE tasks SET journal=?2, journal_arm=?3, explore_json=?4
         WHERE id=?1 AND state IN ('queued', 'blocked')",
        params![id, journal as i64, arm, serde_json::to_string(explore)?],
    )?;
    Ok(n == 1)
}

impl Store {
    /// Insert `t` and draw the arms its id decides, in one transaction:
    /// `draw` runs against the freshly assigned id and returns the
    /// journal flag, the arm name and the explore routing to write
    /// (`queue::assign_journal_arm` and `queue::assign_explore`, whose
    /// draws are pure functions of the id). `cap` enforces a trust
    /// level's `per_day` limit — `(trust, limit, since)` — in the same
    /// transaction as the insert, so two connections racing to file at
    /// the cap cannot both win the way a separate count-then-insert
    /// could (`queue::apply_trust_policy`'s old check); `None` skips it.
    /// Errors,
    /// rolling back the insert, if `draw` errors or if the arm write
    /// matches no row: `t.state` must be `Queued` or `Blocked` (the only
    /// states `enqueue` ever inserts), so a mismatch here means a caller
    /// passed some other state and would otherwise have gone unarmed
    /// silently, the regression this guards against.
    pub fn insert_task_armed(
        &self,
        t: &Task,
        cap: Option<(Trust, i64, i64)>,
        draw: impl FnOnce(i64) -> Result<(bool, String, BTreeMap<String, String>)>,
    ) -> Result<(i64, bool, String, BTreeMap<String, String>)> {
        let mut c = self.lock();
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some((trust, limit, since)) = cap {
            let filed_today = tasks::count_filed_since(&tx, trust, since)?;
            if filed_today >= limit {
                bail!(
                    "trust {}: {filed_today} task(s) filed at this level in the last 24 hours and its per_day limit is {limit}; it can file again when the oldest of those is a day old",
                    trust.as_str()
                );
            }
        }
        let id = tasks::insert_task_row(&tx, t)?;
        let (journal, arm, explore) = draw(id)?;
        if !write_arms(&tx, id, journal, &arm, &explore)? {
            bail!(
                "task {id}: arm write matched no row (state {:?}); insert_task_armed only arms a queued or blocked row",
                t.state.as_str()
            );
        }
        tx.commit()?;
        Ok((id, journal, arm, explore))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(state: TaskState) -> Task {
        Task {
            repo: "/r".into(),
            task: "do".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state,
            ..Default::default()
        }
    }

    #[test]
    fn arms_a_queued_task() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let (id, journal, arm, explore) = store
            .insert_task_armed(&task(TaskState::Queued), None, |id| {
                Ok((id % 2 == 0, "treatment".to_string(), BTreeMap::new()))
            })
            .unwrap();
        let row = store.task(id).unwrap().unwrap();
        assert_eq!(row.journal, journal);
        assert_eq!(row.journal_arm, arm);
        assert_eq!(row.explore, explore);
    }

    #[test]
    fn arms_a_task_inserted_already_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let mut t = task(TaskState::Blocked);
        t.reason = "needs input: which?".to_string();
        let (id, ..) = store
            .insert_task_armed(&t, None, |_| {
                Ok((true, "treatment".to_string(), BTreeMap::new()))
            })
            .unwrap();
        let row = store.task(id).unwrap().unwrap();
        assert_eq!(row.state, TaskState::Blocked);
        assert_eq!(row.journal_arm, "treatment");
    }

    #[test]
    fn refuses_a_state_the_narrow_write_never_matches() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let err = store
            .insert_task_armed(&task(TaskState::Withdrawn), None, |_| {
                Ok((true, "treatment".to_string(), BTreeMap::new()))
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("arm write matched no row"),
            "{err}"
        );
    }

    #[test]
    fn caps_let_exactly_one_concurrent_insert_through_at_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let since = crate::unix_now() - 3600;
        let cap = 3i64;
        let public = || {
            let mut t = task(TaskState::Queued);
            t.trust = Trust::Public;
            t.created_at = crate::unix_now();
            t
        };
        {
            let store = Store::open(&path).unwrap();
            for _ in 0..cap - 1 {
                store
                    .insert_task_armed(&public(), None, |_| {
                        Ok((true, "treatment".to_string(), BTreeMap::new()))
                    })
                    .unwrap();
            }
        }
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let results: Vec<Result<_>> = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let store = Store::open(&path).unwrap();
                    barrier.wait();
                    store.insert_task_armed(&public(), Some((Trust::Public, cap, since)), |_| {
                        Ok((true, "treatment".to_string(), BTreeMap::new()))
                    })
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();
        let ok = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(ok, 1, "{results:?}");
    }

    #[test]
    fn draw_erroring_rolls_back_the_insert() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let before = store.task(1).unwrap();
        assert!(before.is_none());
        let res = store.insert_task_armed(&task(TaskState::Queued), None, |_| bail!("draw failed"));
        assert!(res.is_err());
        assert!(store.task(1).unwrap().is_none(), "insert must roll back");
    }
}
