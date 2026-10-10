//! Watch 3, `steps`: a step kind whose last runs never once succeeded.
//! A kind that fails every time is broken, whatever the tasks were.

use super::{Check, Status, check, clip};
use crate::store::Store;

/// The step kinds watched: attempts by `step`, kernel ops by name.
pub(super) const KINDS: &[&str] = &["code", "review", "verify", "supervisor", "land", "adopt"];
/// How many of a kind's latest runs are read.
pub(super) const WINDOW: usize = 10;
/// Fewer runs than this say nothing yet.
pub(super) const MIN_RUNS: usize = 5;

pub(super) fn watch(store: &Store) -> Check {
    let mut broken = Vec::new();
    let mut seen = Vec::new();
    for kind in KINDS {
        let runs = store.step_runs(kind, WINDOW).unwrap_or_default();
        if runs.is_empty() {
            continue;
        }
        let ok = runs.iter().filter(|r| r.ok).count();
        seen.push(format!("{kind} {ok}/{}", runs.len()));
        if ok == 0 && runs.len() >= MIN_RUNS {
            broken.push(format!(
                "{kind} 0 of its last {} run(s) succeeded (last, task {}: {})",
                runs.len(),
                runs[0].task_id,
                clip(&runs[0].reason, 200)
            ));
        }
    }
    if broken.is_empty() {
        check(
            "steps",
            Status::Ok,
            if seen.is_empty() {
                "no step has run yet".to_string()
            } else {
                format!("succeeded of the last runs: {}", seen.join(", "))
            },
            "",
        )
    } else {
        check(
            "steps",
            Status::Warn,
            broken.join("; "),
            "a step that never succeeds is broken itself: forge trace the last task",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{NOW, attempt, op, store, task};
    use super::*;

    #[test]
    fn five_failed_review_runs_warn() {
        let (_d, s) = store();
        let t = task(&s, "forge", "b", "failed");
        for i in 0..5 {
            attempt(
                &s,
                t,
                "review",
                "agent_failed",
                "no envelope",
                NOW - 100 + i,
            );
        }
        attempt(&s, t, "code", "succeeded", "", NOW);
        let c = watch(&s);
        assert_eq!(c.status, Status::Warn, "{}", c.detail);
        assert!(c.detail.contains("review 0 of its last 5"), "{}", c.detail);
        assert!(!c.detail.contains("code"), "{}", c.detail);
    }

    #[test]
    fn a_land_op_that_failed_ten_times_warns() {
        let (_d, s) = store();
        let t = task(&s, "forge", "b", "failed");
        for i in 0..12 {
            op(&s, t, "land", false, "merge conflict", NOW - 100 + i);
        }
        let c = watch(&s);
        assert_eq!(c.status, Status::Warn, "{}", c.detail);
        assert!(c.detail.contains("land 0 of its last 10"), "{}", c.detail);
    }

    #[test]
    fn one_success_or_too_few_runs_is_ok() {
        let (_d, s) = store();
        let t = task(&s, "forge", "b", "failed");
        for i in 0..6 {
            attempt(&s, t, "review", "agent_failed", "x", NOW - 100 + i);
        }
        attempt(&s, t, "review", "succeeded", "", NOW);
        for i in 0..4 {
            op(&s, t, "verify", false, "x", NOW - 100 + i);
        }
        let c = watch(&s);
        assert_eq!(c.status, Status::Ok, "{}", c.detail);
    }
}
