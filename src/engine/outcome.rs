//! The run's cursor and how a run ends: the bookkeeping every step reads
//! and updates, and the terminal `End` the task's state and reason are
//! derived from.

use super::*;

/// The run's cursor over the resolved steps: where it is, what each
/// directive has spent, what a verifying step or a landing owes a
/// directive as feedback, and which directives are already verified.
/// One `rewind` does every piece of bookkeeping a step back needs; the
/// side effects the caller owns (resetting the tree, reloading the
/// config, refunding an attempt) stay at the call site, named.
pub(super) struct Run {
    /// Hash of the resolved steps `idx` indexes into (`cursor::workflow_hash`).
    pub(super) hash: String,
    pub(super) idx: usize,
    /// The op sequence number of the current step; landing and push
    /// continue from it.
    pub(super) seq: i64,
    /// Attempts used per directive, seeded from the record (`seed_used`).
    pub(super) used: HashMap<i64, i64>,
    /// Feedback owed to a directive by a verifying operation or a landing
    /// that failed after it.
    pub(super) owed: HashMap<i64, String>,
    /// Directives already verified, by sequence number.
    pub(super) done: HashSet<i64>,
}

/// Directives a resumed task may skip: those whose latest attempt
/// succeeded with no later attempt at an earlier step. An attempt at an
/// earlier step after it means the run was rewound past it, so the old
/// success no longer verifies what the tree now holds.
pub(crate) fn resume_done(prior: &[crate::store::Attempt]) -> HashSet<i64> {
    let mut done = HashSet::new();
    let mut floor = i64::MAX;
    for a in prior.iter().rev() {
        if a.step_seq < floor {
            floor = a.step_seq;
            if a.state == AttemptState::Succeeded {
                done.insert(a.step_seq);
            }
        }
    }
    done
}

/// Record where the run stands: the step it is about to run.
pub(super) fn save_cursor(f: &Forge, t: &Task, run: &Run, attempt_no: i64) -> Result<(), Fault> {
    f.store
        .set_run_cursor(t.id, &run.cursor(t, attempt_no).to_json())
        .env()
}

impl Run {
    pub(super) fn cursor(&self, t: &Task, attempt_no: i64) -> RunCursor {
        RunCursor {
            workflow_hash: self.hash.clone(),
            idx: self.idx,
            attempt: attempt_no,
            owed: self.owed.iter().map(|(k, v)| (*k, v.clone())).collect(),
            interface: t.interface.clone(),
            plan: t.plan.clone(),
        }
    }

    pub(super) fn step_seq(&self) -> i64 {
        self.idx as i64 + 1
    }

    pub(super) fn used_at(&self, seq: i64) -> i64 {
        *self.used.get(&seq).unwrap_or(&0)
    }

    /// An attempt that does not count against the directive (refused by
    /// the provider, or a failure that was the test author's).
    pub(super) fn refund(&mut self, f: &Forge, seq: i64, attempt_id: i64) -> Result<(), Fault> {
        *self.used.entry(seq).or_insert(1) -= 1;
        f.store.refund_attempt(attempt_id).env()
    }

    /// Go back to the directive at `to`, owing it `feedback`; everything
    /// verified from there on is unverified again.
    pub(super) fn rewind(&mut self, to: usize, feedback: String) {
        let to_seq = to as i64 + 1;
        self.owed.insert(to_seq, feedback);
        self.done.retain(|&d| d < to_seq);
        self.idx = to;
    }
}

/// How a run ended. Set exactly once at the point that decides it; the
/// push decision and the task's state and reason derive from it, so
/// they cannot disagree.
#[derive(Debug)]
pub(super) enum End {
    /// Every step verified; the branch is pushed for a human (no landing
    /// asked for, or no remote to land on).
    Verified,
    /// Landed on the base at this commit.
    Landed(String),
    /// Verified, pushed and left for a human, with the reason saying why
    /// and how it lands (a capped task's commits that passed the checks).
    Held(String),
    /// Verified work that no agent vouched for, or a review that never
    /// finished: pushed, and a human decides.
    Unverified(String),
    /// The agent stopped with a question, or a reviewer demoted the
    /// task; a demoted branch is pushed so the human can look. `to` is
    /// who the question is addressed to (`None` means the operator).
    Blocked {
        reason: String,
        demoted: bool,
        to: Option<String>,
    },
    /// The task failed. `counted` appends the attempt count to the
    /// reason; `pushes` keeps a verified branch that could not land.
    Failed {
        reason: String,
        counted: bool,
        pushes: bool,
    },
    /// The task's cost cap would be crossed by its next attempt: a
    /// decision, not a failure. `pushes` keeps the branch when an attempt
    /// ran, so a human can read or land what verified.
    Capped { reason: String, pushes: bool },
    /// A plan step with `file_into_initiative` filed its items as
    /// sibling tasks in the task's initiative; nothing changed the tree,
    /// so nothing is pushed. `last` is the last filed task, chained
    /// after every other: `finish` re-points this task's own dependents
    /// at it, since the work they waited for now happens there.
    Filed {
        n: usize,
        initiative: i64,
        last: i64,
    },
}

/// Names the L0 rows the last attempt's verdict failed, the same shape
/// `verify::decide` reports them in ("L0 failed: has-commits"). `None`
/// when nothing at L0 failed, so the caller falls back to the attempt
/// state's own reason (an agent failure or a question carries no rows).
pub(super) fn l0_failure_reason(checks: &[CheckResult]) -> Option<String> {
    let failed: Vec<&str> = checks
        .iter()
        .filter(|c| c.level == "L0" && !c.ok)
        .map(|c| c.name.as_str())
        .collect();
    (!failed.is_empty()).then(|| format!("L0 failed: {}", failed.join(", ")))
}

impl End {
    pub(super) fn pushes(&self) -> bool {
        match self {
            End::Verified | End::Held(_) | End::Unverified(_) => true,
            End::Landed(_) | End::Filed { .. } => false,
            End::Capped { pushes, .. } => *pushes,
            End::Blocked { demoted, .. } => *demoted,
            End::Failed { pushes, .. } => *pushes,
        }
    }

    pub(super) fn task_state(&self) -> TaskState {
        match self {
            End::Verified | End::Landed(_) | End::Held(_) | End::Filed { .. } => {
                TaskState::Succeeded
            }
            End::Unverified(_) => TaskState::Unverified,
            End::Blocked { .. } => TaskState::Blocked,
            End::Failed { .. } => TaskState::Failed,
            End::Capped { .. } => TaskState::Capped,
        }
    }

    /// Who a blocking question is addressed to; `None` for every other
    /// end, and for a blocked one with no addressee (the operator).
    pub(super) fn question_to(&self) -> Option<String> {
        match self {
            End::Blocked { to, .. } => to.clone(),
            _ => None,
        }
    }

    pub(super) fn reason(&self, t: &Task, attempts: usize) -> String {
        match self {
            End::Verified => String::new(),
            End::Landed(sha) => {
                format!("landed {} @ {}", t.base_branch, &sha[..sha.len().min(8)])
            }
            End::Filed { n, initiative, .. } => {
                format!("filed {n} task(s) into initiative {initiative}")
            }
            End::Held(r)
            | End::Unverified(r)
            | End::Blocked { reason: r, .. }
            | End::Capped { reason: r, .. } => r.clone(),
            End::Failed {
                reason, counted, ..
            } => {
                if *counted {
                    format!("{reason} (after {attempts} attempt(s))")
                } else {
                    reason.clone()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_state_maps_every_end_variant() {
        let cases: Vec<(End, TaskState)> = vec![
            (End::Verified, TaskState::Succeeded),
            (End::Landed("abc123".to_string()), TaskState::Succeeded),
            (End::Held("held".to_string()), TaskState::Succeeded),
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
            (End::Held("held for a human".to_string()), 1, "held for a human"),
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
}
