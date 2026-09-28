use crate::checks::CheckResult;
use crate::store::TaskState::{Failed, Succeeded};

/// Each reason's rules as `initiative_hold_detail` feeds them to
/// `same_rule_streak`, for failures that carry no verdict.
fn rules(seq: &[(crate::store::TaskState, &str)]) -> Vec<(crate::store::TaskState, Vec<String>)> {
    seq.iter()
        .map(|(s, r)| (*s, super::failure_rules(r, &[])))
        .collect()
}

/// A failed L1 `test` check whose tail names `tests` as cargo's
/// `failures:` list does, recording no parsed names of its own (so the
/// tail is what the key is read from).
fn failed_test_check(tests: &[&str]) -> CheckResult {
    let mut tail = String::from("test result: FAILED\n\nfailures:\n");
    for t in tests {
        tail.push_str(&format!("    {t}\n"));
    }
    tail.push('\n');
    CheckResult {
        level: "L1".into(),
        name: "test".into(),
        ok: false,
        exit: Some(101),
        tail,
        ..Default::default()
    }
}

#[test]
fn a_streak_survives_a_failure_that_names_the_rule_among_others() {
    let seq = [
        (Failed, "L0 failed: has-commits (after 2 attempt(s))"),
        (Failed, "L0 failed: has-commits (after 2 attempt(s))"),
        (
            Failed,
            "L0 failed: has-commits, changes-match-git (after 2 attempt(s))",
        ),
    ];
    assert_eq!(
        super::same_rule_streak(&rules(&seq)),
        Some(("has-commits".to_string(), 3))
    );
}

#[test]
fn a_landing_or_a_different_rule_ends_the_streak() {
    let broken = [
        (Failed, "L0 failed: has-commits (after 2 attempt(s))"),
        (Succeeded, "landed main @ abc"),
        (Failed, "L0 failed: has-commits (after 2 attempt(s))"),
    ];
    assert_eq!(
        super::same_rule_streak(&rules(&broken)),
        Some(("has-commits".to_string(), 1))
    );
    let other = [
        (Failed, "L0 failed: clean-tree (after 1 attempt(s))"),
        (Failed, "L0 failed: has-commits (after 1 attempt(s))"),
    ];
    assert_eq!(
        super::same_rule_streak(&rules(&other)),
        Some(("has-commits".to_string(), 1))
    );
}

/// Initiative 56, 2026-09-28: tasks 945, 957 and 959 all failed L1
/// `test`, each on a different test (load flakes unrelated to their
/// changes). Keyed by the check and its failing tests, that is three
/// rules, not a streak of three.
#[test]
fn a_check_failing_on_different_tests_is_a_different_rule_each_time() {
    let verdicts = [
        failed_test_check(&["portal::conversation_is_threaded"]),
        failed_test_check(&["worker::window_hold_waits", "worker::window_hold_releases"]),
        failed_test_check(&["plugins::a_held_lock_is_exclusive"]),
    ];
    let seq: Vec<_> = verdicts
        .iter()
        .map(|c| {
            (
                Failed,
                super::failure_rules("L1 failed: test", std::slice::from_ref(c)),
            )
        })
        .collect();
    assert_eq!(
        seq[1].1,
        vec!["L1 test: worker::window_hold_releases, worker::window_hold_waits".to_string()]
    );
    let (rule, len) = super::same_rule_streak(&seq).unwrap();
    assert_eq!(len, 1, "{rule}");
    assert_eq!(rule, "L1 test: plugins::a_held_lock_is_exclusive");
}

/// The same check failing on the same set of tests, whatever order the
/// tests were reported in, is one rule and does make a streak.
#[test]
fn a_check_failing_on_the_same_tests_is_one_rule() {
    let a = failed_test_check(&["a::one", "b::two"]);
    let b = failed_test_check(&["b::two", "a::one"]);
    let mut parsed = failed_test_check(&[]);
    parsed.failing_tests = vec!["a::one".into(), "b::two".into()];
    let seq: Vec<_> = [a, b, parsed]
        .iter()
        .map(|c| {
            (
                Failed,
                super::failure_rules(
                    "L1 failed: test (after 2 attempt(s))",
                    std::slice::from_ref(c),
                ),
            )
        })
        .collect();
    assert_eq!(
        super::same_rule_streak(&seq),
        Some(("L1 test: a::one, b::two".to_string(), 3))
    );
}

/// A failing check that names no tests (clippy, fmt) is keyed by its
/// level and name alone, and an agent failure names no rule at all.
#[test]
fn a_check_with_no_test_names_is_keyed_by_its_name_and_other_failures_by_nothing() {
    let clippy = CheckResult {
        level: "L1".into(),
        name: "clippy".into(),
        ok: false,
        tail: "error: unused variable".into(),
        ..Default::default()
    };
    assert_eq!(
        super::failure_rules("L1 failed: clippy", &[clippy]),
        vec!["L1 clippy".to_string()]
    );
    assert!(super::failure_rules("agent exit 1", &[]).is_empty());
}
