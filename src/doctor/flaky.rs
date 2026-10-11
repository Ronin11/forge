//! `forge doctor`'s "flaky tests" row: test-check failures in the last 24
//! hours whose failing tests all passed again on the base, untouched by the
//! branch that hit them. That is the mechanic's load-flake classification
//! (`supervisor::mechanic::flake`), the signature of a test waiting on a
//! clock the host did not keep, so each one is a decision row of kind
//! `mechanic-load-flake` whose citation names the check and its tests.

use super::{Check, Status, check};
use crate::store::Store;

const WINDOW_SECS: i64 = 24 * 3600;

pub(super) fn check_flaky_tests(store: &Store) -> Vec<Check> {
    let since = crate::unix_now() - WINDOW_SECS;
    let rows = match store.decisions_of_kind_since("mechanic-load-flake", since) {
        Ok(r) => r,
        Err(e) => return vec![check("flaky tests", Status::Fail, format!("{e:#}"), "")],
    };
    // Citations read "<level> <check>: <test>, <test> pass on the base, ...".
    let mut tests: Vec<String> = Vec::new();
    let mut failures = 0;
    for d in &rows {
        let Some((head, rest)) = d.citations.split_once(": ") else {
            continue;
        };
        if head.split_whitespace().nth(1) != Some("test") {
            continue;
        }
        failures += 1;
        let names = rest.split(" pass on the base").next().unwrap_or(rest);
        for name in names.split(", ").map(str::trim).filter(|n| !n.is_empty()) {
            if !tests.iter().any(|t| t == name) {
                tests.push(name.to_string());
            }
        }
    }
    vec![if failures == 0 {
        check(
            "flaky tests",
            Status::Ok,
            "no test-check failure in the last 24h passed again on the base",
            "",
        )
    } else {
        check(
            "flaky tests",
            Status::Warn,
            format!(
                "{failures} test-check failure(s) in the last 24h whose failing tests all pass on the base: {}",
                tests.join(", ")
            ),
            "these tests wait on a clock, not an event: see docs/CHECKS.md, \"Timing in tests\", and `cargo test --test flake -- --nocapture`",
        )
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{InsertDecisionBy, Task};

    fn flake(store: &Store, citation: &str) {
        let id = store
            .insert_task(&Task {
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
            .unwrap();
        let d = store
            .insert_decision_by(InsertDecisionBy {
                task_id: id,
                repo: "r",
                question: "q",
                answer: "a",
                answered_by: "mechanic",
                citations: citation,
                answered_for: None,
            })
            .unwrap();
        store.set_decision_kind(d, "mechanic-load-flake").unwrap();
    }

    #[test]
    fn warns_naming_the_tests_that_pass_on_the_base() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        flake(
            &store,
            "L1 test: worker::a, landing::b pass on the base, untouched by this branch",
        );
        flake(
            &store,
            "L1 test: worker::a pass on the base, untouched by this branch",
        );
        let c = check_flaky_tests(&store);
        assert_eq!(c[0].status, Status::Warn, "{}", c[0].detail);
        assert!(
            c[0].detail.starts_with("2 test-check failure(s)"),
            "{}",
            c[0].detail
        );
        assert!(
            c[0].detail.contains("worker::a, landing::b"),
            "{}",
            c[0].detail
        );
    }

    #[test]
    fn other_checks_and_an_empty_window_are_ok() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        assert_eq!(check_flaky_tests(&store)[0].status, Status::Ok);
        flake(&store, "L0 clippy: x pass on the base, untouched");
        assert_eq!(check_flaky_tests(&store)[0].status, Status::Ok);
    }
}
