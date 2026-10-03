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

impl Store {
    /// Queued tasks whose dependencies have all landed (or succeeded
    /// without landing, when they were told not to) and whose initiative
    /// is not in `held`: what `claim_next` considers, highest priority
    /// first, ties broken oldest first. Moved out of `tasks.rs` to keep
    /// it under its line ceiling (see `tests/file_size.rs`).
    pub fn queued_unblocked(&self, held: &[i64]) -> Result<Vec<Task>> {
        let ids: Vec<i64> = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT t.id FROM tasks t WHERE t.state='queued' AND t.origin='agent' AND NOT EXISTS (
                   SELECT 1 FROM json_each(t.after_json) j LEFT JOIN tasks d ON d.id = j.value
                   WHERE d.id IS NULL OR d.state != 'succeeded' OR (d.land = 1 AND d.landed_sha = '')
                 ) ORDER BY t.priority DESC, t.id ASC",
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
}
