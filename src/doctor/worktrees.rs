//! Finished tasks whose worktree is still on disk (`forge gc` is what
//! removes the published ones and names the rest).

use super::{Check, Status, check};
use crate::store::{Store, TaskState};
use crate::unix_now;

pub(super) fn check_worktrees(store: &Store) -> Vec<Check> {
    let tasks = match store.tasks_with_worktrees() {
        Ok(t) => t,
        Err(e) => return vec![check("worktrees", Status::Fail, format!("{e:#}"), "")],
    };
    let held: Vec<_> = tasks
        .into_iter()
        .filter(|t| t.state != TaskState::Running && t.state != TaskState::Queued)
        .collect();
    let retained: Vec<i64> = held.iter().map(|t| t.id).collect();
    let oldest_days = held
        .iter()
        .filter_map(|t| t.finished_at)
        .min()
        .map(|fin| (unix_now() - fin).max(0) / 86_400);
    vec![if retained.is_empty() {
        check("worktrees", Status::Ok, "none retained", "")
    } else {
        let mut c = check(
            "worktrees",
            Status::Warn,
            format!(
                "{} retained: {:?}, oldest {}d",
                retained.len(),
                retained,
                oldest_days.unwrap_or(0)
            ),
            "forge gc removes the published ones and explains the rest",
        );
        c.worktree_ids = Some(retained);
        c
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Task;

    fn fixture_task(state: TaskState, worktree: &str, finished_at: Option<i64>) -> Task {
        Task {
            repo: "/repo".into(),
            task: "do the thing".into(),
            base_branch: "main".into(),
            model: "sonnet".into(),
            max_turns: 10,
            max_attempts: 1,
            timeout_secs: 60,
            state,
            worktree: worktree.into(),
            finished_at,
            created_at: unix_now(),
            workflow: "direct".into(),
            ..Default::default()
        }
    }

    #[test]
    fn check_worktrees_is_ok_with_no_retained_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("forge.db")).unwrap();
        let checks = check_worktrees(&store);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "worktrees");
        assert!(checks[0].status == Status::Ok);
        assert_eq!(checks[0].detail, "none retained");
    }

    /// A finished task whose worktree is still on disk: WARN, and the
    /// structured `worktree_ids` a client (the doctor page's gc control)
    /// reads instead of parsing the `{:?}`-formatted list out of `detail`.
    #[test]
    fn check_worktrees_warns_and_carries_the_retained_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("forge.db")).unwrap();
        let mut t = fixture_task(TaskState::Failed, "/wt/1", Some(unix_now()));
        t.id = store.insert_task(&t).unwrap();
        store.update_task(&t).unwrap();

        let checks = check_worktrees(&store);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "worktrees");
        assert!(checks[0].status == Status::Warn);
        assert_eq!(checks[0].worktree_ids, Some(vec![t.id]));
        assert!(
            checks[0].detail.contains(&t.id.to_string()),
            "{}",
            checks[0].detail
        );
    }
}
