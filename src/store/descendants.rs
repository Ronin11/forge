//! The live tasks below and beside a task in its retry lineage: what
//! `forge retry` refuses to duplicate, `forge show` lists, and
//! `forge withdraw --abort` weighs a running task against.

use super::*;

/// Whether a task in this state (as `TaskState::as_str` spells it) is
/// still going somewhere: queued, running, or awaiting a human's look.
pub fn is_live_state(state: &str) -> bool {
    matches!(state, "queued" | "running" | "unverified")
}

/// Every live task below `id` in `rows` (one lineage, see `Store::lineage`),
/// through retries of retries, in id order: a `forge retry` or a review
/// demotion's follow-up (both are `retry_of` children) that has not ended.
pub fn live_descendants(rows: &[LineageRow], id: i64) -> Vec<&LineageRow> {
    let mut below = vec![id];
    let mut found = Vec::new();
    while let Some(at) = below.pop() {
        for r in rows.iter().filter(|r| r.parent == Some(at)) {
            below.push(r.id);
            if is_live_state(&r.state) {
                found.push(r);
            }
        }
    }
    found.sort_by_key(|r| r.id);
    found
}

/// The live tasks that retry the same task `id` does, other than `id`
/// itself: the ones `id` may be a duplicate of.
pub fn live_siblings(rows: &[LineageRow], id: i64) -> Vec<&LineageRow> {
    let Some(parent) = rows.iter().find(|r| r.id == id).and_then(|r| r.parent) else {
        return Vec::new();
    };
    rows.iter()
        .filter(|r| r.id != id && r.parent == Some(parent) && is_live_state(&r.state))
        .collect()
}

impl Store {
    /// The live tasks below `id` in its lineage (see `live_descendants`).
    pub fn live_descendants(&self, id: i64) -> Result<Vec<LineageRow>> {
        let rows = self.lineage(id)?;
        Ok(live_descendants(&rows, id).into_iter().cloned().collect())
    }

    /// The live tasks that retry what `id` retries (see `live_siblings`).
    pub fn live_siblings(&self, id: i64) -> Result<Vec<LineageRow>> {
        let rows = self.lineage(id)?;
        Ok(live_siblings(&rows, id).into_iter().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Task;

    fn row(id: i64, parent: Option<i64>, state: &str) -> LineageRow {
        LineageRow {
            id,
            parent,
            supersedes: None,
            state: state.into(),
            reason: String::new(),
            workflow: "w".into(),
            cost: 0.0,
            land: true,
            landed_sha: String::new(),
        }
    }

    #[test]
    fn live_descendants_names_a_demotion_followup_and_a_hand_retry_of_the_same_task() {
        // 811 blocked; the demotion rule filed 822, `forge retry 811` filed 823.
        let rows = [
            row(811, None, "blocked"),
            row(822, Some(811), "running"),
            row(823, Some(811), "queued"),
        ];
        let ids: Vec<i64> = live_descendants(&rows, 811).iter().map(|r| r.id).collect();
        assert_eq!(ids, [822, 823]);
    }

    #[test]
    fn live_descendants_reach_through_an_ended_retry_and_skip_ended_ones() {
        let rows = [
            row(1, None, "failed"),
            row(2, Some(1), "failed"),
            row(3, Some(2), "unverified"),
            row(4, Some(1), "succeeded"),
            row(5, Some(1), "withdrawn"),
            row(6, Some(1), "capped"),
            row(7, Some(1), "blocked"),
        ];
        let ids: Vec<i64> = live_descendants(&rows, 1).iter().map(|r| r.id).collect();
        assert_eq!(ids, [3]);
        assert!(live_descendants(&rows, 3).is_empty());
    }

    #[test]
    fn live_descendants_ignore_ancestors_and_siblings() {
        let rows = [
            row(1, None, "running"),
            row(2, Some(1), "failed"),
            row(3, Some(1), "queued"),
        ];
        assert!(live_descendants(&rows, 2).is_empty());
    }

    #[test]
    fn live_siblings_are_the_other_live_tasks_retrying_the_same_parent() {
        let rows = [
            row(811, None, "blocked"),
            row(822, Some(811), "running"),
            row(823, Some(811), "running"),
            row(824, Some(811), "failed"),
            row(830, Some(822), "queued"),
        ];
        let ids: Vec<i64> = live_siblings(&rows, 823).iter().map(|r| r.id).collect();
        assert_eq!(ids, [822]);
        assert!(
            live_siblings(&rows, 811).is_empty(),
            "a root has no siblings"
        );
        assert!(live_siblings(&rows, 830).is_empty());
    }

    #[test]
    fn the_store_reads_live_descendants_and_an_abort_decision() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let root = queued_task(&s);
        let mut a = s.task(queued_task(&s)).unwrap().unwrap();
        a.retry_of = Some(root);
        s.update_task(&a).unwrap();
        let mut b = s.task(queued_task(&s)).unwrap().unwrap();
        b.retry_of = Some(root);
        s.update_task(&b).unwrap();
        let ids: Vec<i64> = s
            .live_descendants(root)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, [a.id, b.id]);
        let sib: Vec<i64> = s
            .live_siblings(b.id)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(sib, [a.id]);

        assert_eq!(s.abort_requested(b.id).unwrap(), None);
        let d = s
            .insert_decision_by(InsertDecisionBy {
                task_id: b.id,
                repo: "r",
                question: "q",
                answer: "duplicate",
                answered_by: "operator",
                citations: "",
                answered_for: None,
            })
            .unwrap();
        assert_eq!(s.abort_requested(b.id).unwrap(), None, "kind not set yet");
        s.set_decision_kind(d, "withdraw-abort").unwrap();
        assert_eq!(
            s.abort_requested(b.id).unwrap().as_deref(),
            Some("duplicate")
        );
        assert_eq!(s.abort_requested(a.id).unwrap(), None);
    }

    fn queued_task(s: &Store) -> i64 {
        s.insert_task(&Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 2,
            timeout_secs: 1,
            ..Default::default()
        })
        .unwrap()
    }
}
