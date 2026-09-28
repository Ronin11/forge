use super::*;

#[test]
fn task_state_maps_every_end_variant() {
    let cases: Vec<(End, TaskState)> = vec![
        (End::Verified, TaskState::Succeeded),
        (End::Landed("abc123".to_string()), TaskState::Succeeded),
        (End::Unverified("reason".to_string()), TaskState::Unverified),
        (
            End::Blocked {
                reason: "reason".to_string(),
                demoted: false,
                to: None,
            },
            TaskState::Blocked,
        ),
        (
            End::Blocked {
                reason: "reason".to_string(),
                demoted: true,
                to: None,
            },
            TaskState::Blocked,
        ),
        (
            End::Failed {
                reason: "reason".to_string(),
                counted: true,
                pushes: false,
            },
            TaskState::Failed,
        ),
        (
            // The cap would be crossed: a decision, never a failure,
            // whether or not the code step had verified.
            End::Capped {
                reason: "$5.12 of $5.00; code step verified, review not run".to_string(),
                pushes: true,
            },
            TaskState::Capped,
        ),
    ];
    for (end, expected) in cases {
        assert_eq!(end.task_state(), expected, "{end:?} -> {expected:?}");
    }
}

fn check(level: &str, name: &str, ok: bool) -> CheckResult {
    CheckResult {
        level: level.into(),
        name: name.into(),
        ok,
        ..Default::default()
    }
}

#[test]
fn l0_failure_reason_names_failing_l0_rows_and_none_otherwise() {
    assert_eq!(l0_failure_reason(&[]), None);
    assert_eq!(
        l0_failure_reason(&[check("L0", "clean-tree", true)]),
        None,
        "an L0 row that passed names nothing"
    );
    assert_eq!(
        l0_failure_reason(&[check("L1", "tests", false)]),
        None,
        "a failing row outside L0 does not count"
    );
    assert_eq!(
        l0_failure_reason(&[
            check("L0", "clean-tree", true),
            check("L0", "has-commits", false)
        ]),
        Some("L0 failed: has-commits".to_string())
    );
}

#[test]
fn reason_maps_every_end_variant() {
    let t = Task::default();
    let cases: Vec<(End, usize, &str)> = vec![
        (End::Verified, 1, ""),
        (
            End::Landed("abc123def".to_string()),
            1,
            "landed  @ abc123de",
        ),
        (End::Unverified("reason".to_string()), 1, "reason"),
        (
            End::Blocked {
                reason: "needs input: which one?".to_string(),
                demoted: false,
                to: None,
            },
            1,
            "needs input: which one?",
        ),
        (
            End::Failed {
                reason: "operation setup failed: exit 1".to_string(),
                counted: false,
                pushes: false,
            },
            3,
            "operation setup failed: exit 1",
        ),
        (
            End::Failed {
                reason: "some failure".to_string(),
                counted: true,
                pushes: false,
            },
            4,
            "some failure (after 4 attempt(s))",
        ),
        (
            // A landing rewind sent the coder back to commit again; it
            // made none, so the checks failed on has-commits with no
            // agent failure to explain it. The reason built after the
            // attempt loop must name the failing rule, never come out
            // empty (task 232's bug).
            End::Failed {
                reason: l0_failure_reason(&[
                    check("L0", "clean-tree", true),
                    check("L0", "has-commits", false),
                ])
                .expect("has-commits failed"),
                counted: true,
                pushes: false,
            },
            4,
            "L0 failed: has-commits (after 4 attempt(s))",
        ),
    ];
    for (end, attempts, expected) in cases {
        assert_eq!(end.reason(&t, attempts), expected, "{end:?}");
        assert!(
            !end.reason(&t, attempts).is_empty() || matches!(end, End::Verified),
            "a non-Verified end must never carry an empty reason: {end:?}"
        );
    }
}

fn attempts(steps: &[(i64, AttemptState)]) -> Vec<crate::store::Attempt> {
    steps
        .iter()
        .enumerate()
        .map(|(i, &(step_seq, state))| crate::store::Attempt {
            attempt_no: i as i64 + 1,
            step_seq,
            state,
            ..Default::default()
        })
        .collect()
}

#[test]
fn resume_done_counts_steps_whose_latest_attempt_succeeded() {
    let prior = attempts(&[(1, AttemptState::Succeeded), (2, AttemptState::Succeeded)]);
    assert_eq!(resume_done(&prior), HashSet::from([1, 2]));
}

#[test]
fn resume_done_forgets_verifications_a_rewind_passed() {
    let prior = attempts(&[
        (1, AttemptState::Succeeded),
        (2, AttemptState::Succeeded),
        (1, AttemptState::AgentFailed),
    ]);
    assert!(resume_done(&prior).is_empty());
    let prior = attempts(&[
        (1, AttemptState::Succeeded),
        (2, AttemptState::Succeeded),
        (1, AttemptState::Succeeded),
    ]);
    assert_eq!(resume_done(&prior), HashSet::from([1]));
}

#[test]
fn resume_done_uses_the_latest_attempt_at_a_step() {
    let prior = attempts(&[(1, AttemptState::AgentFailed), (1, AttemptState::Succeeded)]);
    assert_eq!(resume_done(&prior), HashSet::from([1]));
    let prior = attempts(&[(1, AttemptState::Succeeded), (1, AttemptState::AgentFailed)]);
    assert!(resume_done(&prior).is_empty());
}
