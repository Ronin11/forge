//! `forge doctor`'s "dangling" row: lineages whose tip failed or blocked
//! with no open question, in the last 7 days (see `crate::lineage`).

use super::{Check, Status, check};
use crate::lineage::{self, Outcome};
use crate::store::Store;

const WINDOW_SECS: i64 = 7 * 24 * 3600;

pub(super) fn check_dangling(store: &Store) -> Vec<Check> {
    let since = crate::unix_now() - WINDOW_SECS;
    let roots = match store.roots_since(since) {
        Ok(r) => r,
        Err(e) => return vec![check("dangling", Status::Fail, format!("{e:#}"), "")],
    };
    let mut tips = Vec::new();
    for root in roots {
        let rows = match store.lineage(root) {
            Ok(r) => r,
            Err(e) => return vec![check("dangling", Status::Fail, format!("{e:#}"), "")],
        };
        if let Some(l) = lineage::lineage_of(&rows)
            && l.outcome == Outcome::Dangling
        {
            tips.push(format!("task {}: {}", l.tip, l.reason));
        }
    }
    vec![if tips.is_empty() {
        check(
            "dangling",
            Status::Ok,
            "no dangling lineages in the last 7 days",
            "",
        )
    } else {
        check(
            "dangling",
            Status::Warn,
            format!(
                "{} dangling tip(s) in the last 7 days: {}",
                tips.len(),
                tips.join("; ")
            ),
            "forge audit --since 7d for the full lineage; each tip needs a human decision: retry, withdraw, or answer its question",
        )
    }]
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
    fn warns_on_a_dangling_tip_in_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let id = queued(&store);
        let mut t = store.task(id).unwrap().unwrap();
        t.state = crate::store::TaskState::Failed;
        t.reason = "agent exit 1".into();
        store.update_task(&t).unwrap();

        let checks = check_dangling(&store);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, Status::Warn, "{}", checks[0].detail);
        assert!(checks[0].detail.contains(&format!("task {id}")));
        assert!(checks[0].detail.contains("agent exit 1"));
    }

    #[test]
    fn a_question_blocked_task_does_not_warn() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let id = queued(&store);
        let mut t = store.task(id).unwrap().unwrap();
        t.state = crate::store::TaskState::Blocked;
        t.reason = "needs input: which db?".into();
        store.update_task(&t).unwrap();

        let checks = check_dangling(&store);
        assert_eq!(checks[0].status, Status::Ok, "{}", checks[0].detail);
    }

    #[test]
    fn ok_with_nothing_dangling() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        queued(&store);
        let checks = check_dangling(&store);
        assert_eq!(checks[0].status, Status::Ok);
    }
}
