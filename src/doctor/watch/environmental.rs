//! Watch 4, `environmental`: one named check failing on every task of a
//! project that ran it in the last day, across at least three branches.
//! Three unrelated changes cannot all break the same check; the machine
//! did (the `provision` test under mise on 10-09 and 10-10).

use super::{Check, DAY, Status, check, clip};
use crate::checks::CheckResult;
use crate::store::Store;
use std::collections::{BTreeMap, BTreeSet};

/// Fewer branches than this could still be the agents.
pub(super) const MIN_BRANCHES: usize = 3;

/// Per (project, check): each task's branch, whether the check ever
/// passed on it, and the newest failing tail.
#[derive(Default)]
struct Seen {
    tasks: BTreeMap<i64, (String, bool)>,
    tail: String,
}

pub(super) fn watch(store: &Store, now: i64) -> Check {
    let rows = match store.verdicts_since(now - DAY) {
        Ok(r) => r,
        Err(e) => return check("environmental", Status::Warn, format!("{e:#}"), ""),
    };
    let mut seen: BTreeMap<(String, String), Seen> = BTreeMap::new();
    for row in rows.iter().filter(|r| !r.project.is_empty()) {
        let verdict: Vec<CheckResult> = serde_json::from_str(&row.verdict_json).unwrap_or_default();
        for c in verdict {
            let e = seen.entry((row.project.clone(), c.name.clone())).or_default();
            let t = e
                .tasks
                .entry(row.task_id)
                .or_insert_with(|| (row.branch.clone(), false));
            t.1 |= c.ok;
            if !c.ok {
                e.tail = c.failure_text();
            }
        }
    }
    let mut found = Vec::new();
    for ((project, name), s) in &seen {
        let branches: BTreeSet<&str> = s.tasks.values().map(|(b, _)| b.as_str()).collect();
        if branches.len() >= MIN_BRANCHES && s.tasks.values().all(|(_, passed)| !passed) {
            found.push(format!(
                "check {name} failed on every task of {project} in the last 24h, across {} \
                 branches ({}): environmental, not the agents; its tail: {}",
                branches.len(),
                branches.into_iter().collect::<Vec<_>>().join(", "),
                clip(&s.tail, 400)
            ));
        }
    }
    if found.is_empty() {
        check(
            "environmental",
            Status::Ok,
            "no check failed on every task of a project across 3 or more branches in the last 24h",
            "",
        )
    } else {
        check(
            "environmental",
            Status::Warn,
            found.join("; "),
            "run the check by hand on the base branch: the toolchain, a cache or the network is \
             what broke, and retrying the tasks will not fix it",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{NOW, attempt_with, store, task};
    use super::*;

    fn verdict(name: &str, ok: bool, tail: &str) -> String {
        serde_json::to_string(&vec![CheckResult {
            level: "L1".into(),
            name: name.into(),
            ok,
            tail: tail.into(),
            ..Default::default()
        }])
        .unwrap()
    }

    #[test]
    fn one_check_failing_on_three_branches_of_a_project_warns_with_its_tail() {
        let (_d, s) = store();
        for b in ["a", "b", "c"] {
            let t = task(&s, "forge", b, "failed");
            let v = verdict("provision", false, "mise: tool not found");
            attempt_with(&s, t, "code", "checks_failed", "", NOW - 600, &v, "{}");
        }
        let c = watch(&s, NOW);
        assert_eq!(c.status, Status::Warn, "{}", c.detail);
        assert!(c.detail.contains("check provision"), "{}", c.detail);
        assert!(c.detail.contains("environmental, not the agents"), "{}", c.detail);
        assert!(c.detail.contains("mise: tool not found"), "{}", c.detail);
    }

    #[test]
    fn a_pass_on_one_task_or_only_two_branches_is_ok() {
        let (_d, s) = store();
        for (b, ok) in [("a", false), ("b", false), ("c", true)] {
            let t = task(&s, "forge", b, "failed");
            let v = verdict("provision", ok, "x");
            attempt_with(&s, t, "code", "checks_failed", "", NOW - 600, &v, "{}");
        }
        assert_eq!(watch(&s, NOW).status, Status::Ok);

        let (_d, s) = store();
        for b in ["a", "b", "b"] {
            let t = task(&s, "forge", b, "failed");
            let v = verdict("provision", false, "x");
            attempt_with(&s, t, "code", "checks_failed", "", NOW - 600, &v, "{}");
        }
        assert_eq!(watch(&s, NOW).status, Status::Ok);
    }
}
