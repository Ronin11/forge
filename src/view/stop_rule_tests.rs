use crate::store::TaskState::{Failed, Succeeded};

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
        super::same_rule_streak(&seq),
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
        super::same_rule_streak(&broken),
        Some(("has-commits".to_string(), 1))
    );
    let other = [
        (Failed, "L0 failed: clean-tree (after 1 attempt(s))"),
        (Failed, "L0 failed: has-commits (after 1 attempt(s))"),
    ];
    assert_eq!(
        super::same_rule_streak(&other),
        Some(("has-commits".to_string(), 1))
    );
}
