//! Task priority: an integer, 0 (lowest) to 7 (highest), 2 ("normal") by
//! default (the column's own `CHECK` and `DEFAULT`, see the migration
//! ladder). Among otherwise-claimable tasks, the claim order is priority
//! descending, then id ascending (`Store::queued_unblocked`, what
//! `Store::claim_next` walks). A retry or a refile inherits the task it
//! re-queues' own value (`queue::retry_request`), never this default.

use super::*;

pub const PRIORITY_MIN: i64 = 0;
pub const PRIORITY_MAX: i64 = 7;
pub const PRIORITY_DEFAULT: i64 = 2;

/// `forge add`, `forge task set`, `forge initiative new` and `forge
/// initiative set --priority`: a plain integer 0-7, or one of the
/// aliases low=1, normal=2, high=5, urgent=7. A clap `value_parser`
/// (hence the `String` error, not `anyhow::Error`), so a bad value is
/// refused at argument-parsing time, before any request is built.
pub fn parse_priority(s: &str) -> std::result::Result<i64, String> {
    let n = match s.trim().to_ascii_lowercase().as_str() {
        "low" => 1,
        "normal" => 2,
        "high" => 5,
        "urgent" => 7,
        other => other.parse::<i64>().map_err(|_| {
            format!(
                "--priority {s:?}: must be an integer between {PRIORITY_MIN} and {PRIORITY_MAX}, or one of low, normal, high, urgent"
            )
        })?,
    };
    if !(PRIORITY_MIN..=PRIORITY_MAX).contains(&n) {
        return Err(format!(
            "--priority {n}: must be between {PRIORITY_MIN} and {PRIORITY_MAX}"
        ));
    }
    Ok(n)
}

/// A dependency (`j` over `after_json`, `d` its task row) that keeps its
/// dependent from being claimed. Shared by `queued_unblocked` and
/// `queue_breakdown` so the two cannot disagree.
const UNMET_DEP: &str =
    "d.id IS NULL OR d.state != 'succeeded' OR (d.land = 1 AND d.landed_sha = '')";

/// Why a queued task is, or is not, claimable (`Store::queue_breakdown`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // consumed by doctor and the idle worker
pub enum QueueStatus {
    Claimable,
    /// The first unmet dependency is queued or running (or otherwise not
    /// settled: failed, missing, or succeeded but not yet landed).
    WaitsOnActive {
        dep: i64,
    },
    /// The first unmet dependency is blocked, awaiting an answer.
    WaitsOnBlocked {
        dep: i64,
    },
    HeldInitiative {
        initiative: i64,
    },
}

impl Store {
    /// Every queued agent task with the reason it is or is not claimable,
    /// in claim order. A held initiative is reported ahead of an unmet
    /// dependency; `Claimable` is exactly `queued_unblocked(held)`.
    #[allow(dead_code)] // consumed by doctor and the idle worker
    pub fn queue_breakdown(&self, held: &[i64]) -> Result<Vec<(i64, QueueStatus)>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT t.id, t.initiative,
               (SELECT d.id FROM json_each(t.after_json) j LEFT JOIN tasks d ON d.id = j.value
                 WHERE {UNMET_DEP} ORDER BY j.key LIMIT 1) AS dep_id,
               (SELECT d.state FROM json_each(t.after_json) j LEFT JOIN tasks d ON d.id = j.value
                 WHERE {UNMET_DEP} ORDER BY j.key LIMIT 1) AS dep_state,
               (SELECT j.value FROM json_each(t.after_json) j LEFT JOIN tasks d ON d.id = j.value
                 WHERE {UNMET_DEP} ORDER BY j.key LIMIT 1) AS dep_raw
             FROM tasks t WHERE t.state='queued' AND t.origin='agent'
             ORDER BY t.priority DESC, t.id ASC"
        ))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>("id")?,
                    r.get::<_, Option<i64>>("initiative")?,
                    r.get::<_, Option<i64>>("dep_id")?,
                    r.get::<_, Option<String>>("dep_state")?,
                    r.get::<_, Option<i64>>("dep_raw")?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .map(|(id, initiative, dep, state, raw)| {
                let status = match (initiative.filter(|i| held.contains(i)), dep.or(raw)) {
                    (Some(initiative), _) => QueueStatus::HeldInitiative { initiative },
                    (None, Some(dep)) if state.as_deref() == Some("blocked") => {
                        QueueStatus::WaitsOnBlocked { dep }
                    }
                    (None, Some(dep)) => QueueStatus::WaitsOnActive { dep },
                    (None, None) => QueueStatus::Claimable,
                };
                (id, status)
            })
            .collect())
    }

    /// Queued tasks whose dependencies have all landed (or succeeded
    /// without landing, when they were told not to) and whose initiative
    /// is not in `held`: what `claim_next` considers, highest priority
    /// first, ties broken oldest first. Moved out of `tasks.rs` to keep
    /// it under its line ceiling (see `tests/file_size.rs`).
    pub fn queued_unblocked(&self, held: &[i64]) -> Result<Vec<Task>> {
        let ids: Vec<i64> = {
            let c = self.lock();
            let mut stmt = c.prepare(
                &format!(
                    "SELECT t.id FROM tasks t WHERE t.state='queued' AND t.origin='agent' AND NOT EXISTS (
                   SELECT 1 FROM json_each(t.after_json) j LEFT JOIN tasks d ON d.id = j.value
                   WHERE {UNMET_DEP}
                 ) ORDER BY t.priority DESC, t.id ASC"
                ),
            )?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut out = Vec::new();
        for id in ids {
            let Some(t) = self.task(id)? else { continue };
            if !t.initiative.is_some_and(|i| held.contains(&i)) {
                out.push(t);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(s: &Store, priority: i64, after: Vec<i64>) -> i64 {
        s.insert_task(&Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            priority,
            after,
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn claim_next_orders_by_priority_then_id_and_respects_unmet_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let low = insert(&s, 2, vec![]);
        let urgent = insert(&s, 7, vec![]);
        let blocked_on_low = insert(&s, 7, vec![low]);
        let high = insert(&s, 5, vec![]);

        assert_eq!(
            s.claim_next(1, &[], |_| false).unwrap().map(|t| t.id),
            Some(urgent),
            "priority 7 claims before 5 and 2; blocked_on_low is also priority 7 but still waits on `low`"
        );
        assert_eq!(
            s.claim_next(2, &[], |_| false).unwrap().map(|t| t.id),
            Some(high),
            "priority 5 is next"
        );
        assert_eq!(
            s.claim_next(3, &[], |_| false).unwrap().map(|t| t.id),
            Some(low),
            "priority 2 is the only one left ready"
        );
        assert!(
            s.claim_next(4, &[], |_| false).unwrap().is_none(),
            "blocked_on_low still waits on `low`, which has not succeeded yet"
        );

        let mut t = s.task(low).unwrap().unwrap();
        t.state = TaskState::Succeeded;
        s.update_task(&t).unwrap();
        assert_eq!(
            s.claim_next(5, &[], |_| false).unwrap().map(|t| t.id),
            Some(blocked_on_low),
            "once `low` succeeds, its dependent becomes claimable"
        );
    }

    #[test]
    fn the_check_constraint_refuses_a_priority_outside_zero_to_seven() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let id = insert(&s, PRIORITY_DEFAULT, vec![]);
        let err = s
            .lock()
            .execute("UPDATE tasks SET priority=8 WHERE id=?1", [id])
            .unwrap_err()
            .to_string();
        assert!(err.contains("CHECK"), "{err}");
    }

    #[test]
    fn parse_priority_accepts_digits_and_aliases_and_refuses_out_of_range() {
        assert_eq!(parse_priority("0"), Ok(0));
        assert_eq!(parse_priority("7"), Ok(7));
        assert_eq!(parse_priority("low"), Ok(1));
        assert_eq!(parse_priority("NORMAL"), Ok(2));
        assert_eq!(parse_priority("high"), Ok(5));
        assert_eq!(parse_priority("urgent"), Ok(7));
        let err = parse_priority("8").unwrap_err();
        assert!(err.contains('8') && err.contains('7'), "{err}");
        assert!(parse_priority("bogus").is_err());
    }

    fn set_state(s: &Store, id: i64, state: TaskState) {
        let mut t = s.task(id).unwrap().unwrap();
        t.state = state;
        s.update_task(&t).unwrap();
    }

    fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        (dir, s)
    }

    fn status(s: &Store, id: i64, held: &[i64]) -> QueueStatus {
        let all = s.queue_breakdown(held).unwrap();
        all.into_iter().find(|(i, _)| *i == id).unwrap().1
    }

    #[test]
    fn breakdown_claimable_matches_queued_unblocked_over_every_case() {
        let (_d, s) = open();
        let free = insert(&s, 2, vec![]);
        let running = insert(&s, 2, vec![]);
        set_state(&s, running, TaskState::Running);
        let blocked = insert(&s, 2, vec![]);
        set_state(&s, blocked, TaskState::Blocked);
        let done = insert(&s, 2, vec![]);
        set_state(&s, done, TaskState::Succeeded);
        let queued_dep = insert(&s, 3, vec![free]);
        let running_dep = insert(&s, 4, vec![running]);
        let blocked_dep = insert(&s, 5, vec![blocked]);
        let done_dep = insert(&s, 6, vec![done]);
        let held = insert(&s, 7, vec![]);
        s.lock()
            .execute("UPDATE tasks SET initiative=9 WHERE id=?1", [held])
            .unwrap();
        let _ = (queued_dep, running_dep, blocked_dep, done_dep);

        let claimable: Vec<i64> = s
            .queue_breakdown(&[9])
            .unwrap()
            .into_iter()
            .filter(|(_, st)| *st == QueueStatus::Claimable)
            .map(|(i, _)| i)
            .collect();
        let expect: Vec<i64> = s
            .queued_unblocked(&[9])
            .unwrap()
            .iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(claimable, expect);
        assert_eq!(claimable, vec![done_dep, free]);
    }

    #[test]
    fn breakdown_claimable() {
        let (_d, s) = open();
        let a = insert(&s, 2, vec![]);
        assert_eq!(status(&s, a, &[]), QueueStatus::Claimable);
    }

    #[test]
    fn breakdown_waits_on_active() {
        let (_d, s) = open();
        let a = insert(&s, 2, vec![]);
        let b = insert(&s, 2, vec![a]);
        assert_eq!(status(&s, b, &[]), QueueStatus::WaitsOnActive { dep: a });
        set_state(&s, a, TaskState::Running);
        assert_eq!(status(&s, b, &[]), QueueStatus::WaitsOnActive { dep: a });
    }

    #[test]
    fn breakdown_waits_on_blocked() {
        let (_d, s) = open();
        let a = insert(&s, 2, vec![]);
        set_state(&s, a, TaskState::Blocked);
        let b = insert(&s, 2, vec![a]);
        assert_eq!(status(&s, b, &[]), QueueStatus::WaitsOnBlocked { dep: a });
    }

    #[test]
    fn breakdown_held_initiative() {
        let (_d, s) = open();
        let a = insert(&s, 2, vec![]);
        s.lock()
            .execute("UPDATE tasks SET initiative=4 WHERE id=?1", [a])
            .unwrap();
        assert_eq!(
            status(&s, a, &[4]),
            QueueStatus::HeldInitiative { initiative: 4 }
        );
        assert_eq!(status(&s, a, &[]), QueueStatus::Claimable);
    }
}
