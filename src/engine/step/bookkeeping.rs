//! Refund and rewind a coder when verification failed only in the tests author's namespace.
use super::attempts::{AttemptFlow, AttemptLoop};
use super::*;

pub(super) async fn rewind_tests(
    args: &mut RunDirectiveStep<'_>,
    a: &crate::store::Attempt,
    verdict: &verify::Verdict,
) -> Result<Option<StepFlow>, Fault> {
    let f = args.f;
    let t = &mut *args.t;
    let cfg = args.cfg;
    let resolved = args.resolved;
    let run = &mut *args.run;
    let step = args.step;
    let seq = args.seq;
    let id = t.id;
    // A check that failed only inside the verification namespace
    // is the test author's failure, not the coder's: the coder
    // cannot see those files. Back to the tests step, within its
    // attempts; this attempt does not count against the coder.
    if a.state == AttemptState::ChecksFailed
        && step.action.contract != Contract::Tests
        && let Some((check, tail)) = verify::tests_fault(&verdict.checks, &cfg.namespace)
        && let Some(t_idx) = (0..run.idx)
            .rev()
            .find(|&i| resolved.steps[i].action.contract == Contract::Tests)
    {
        let t_seq = t_idx as i64 + 1;
        let t_used = run.used_at(t_seq);
        if t_used < t.max_attempts {
            run.refund(f, seq, a.id)?;
            f.report.emit(
                id,
                Event::Note {
                    text: &format!(
                        "verify   {check} failed inside {}; back to {} for another attempt",
                        cfg.namespace.join(" "),
                        resolved.steps[t_idx].action.name
                    ),
                },
            );
            run.rewind(t_idx, format!("The repository's `{check}` check failed on the implementer's tree, and every error is inside your tests:\n{tail}\nThe implementer cannot see or edit those files. Fix your tests so the repository's checks pass with them in place, commit, and describe the interface again."));
            // The coder starts over against the corrected tests.
            git::reset_hard(Path::new(&t.worktree), &t.base_sha)
                .await
                .task()?;
            return Ok(Some(StepFlow::Again));
        }
        return Ok(Some(StepFlow::End(End::Failed {
            reason: format!(
                "check {check} failed inside the verification namespace after {t_used} tests attempt(s): {}",
                tail.lines().next().unwrap_or("")
            ),
            counted: true,
            pushes: false,
        })));
    }
    Ok(None)
}

pub(super) async fn retry_after_attempt(
    args: &mut RunDirectiveStep<'_>,
    state: &mut AttemptLoop,
    ts: &Task,
    a: &crate::store::Attempt,
    verdict: &verify::Verdict,
    outcome: &crate::agent::Outcome,
) -> Result<Option<AttemptFlow>, Fault> {
    let f = args.f;
    let t = &mut *args.t;
    let cfg = args.cfg;
    let run = &mut *args.run;
    let step = args.step;
    let seq = args.seq;
    let id = t.id;
    // The provider refused the run: not an attempt the agent
    // spent. The hold at the top of the loop waits for the
    // window, or for a refused login to answer a probe; the same
    // feedback and session go again.
    if outcome.rate_limited && a.state != AttemptState::Unverified {
        if outcome.login_refused {
            crate::login_hold::hold(f, &ts.provider, outcome, id).env()?;
        }
        state.consecutive_refusals += 1;
        if state.consecutive_refusals > REFUSAL_LIMIT {
            return Ok(Some(AttemptFlow::Exit(StepFlow::End(refusal_exhausted()))));
        }
        f.report.emit(
            id,
            Event::Note {
                text: "rate     the provider refused this run; it does not count as an attempt",
            },
        );
        run.refund(f, seq, a.id)?;
        return Ok(Some(AttemptFlow::Continue));
    }
    state.consecutive_refusals = 0;
    // An environment need the policy covers (a refused host, a host cache) is
    // applied and rerun without counting; anything else falls through.
    match environment_after(f, t, cfg, a, verdict).await? {
        Environment::Applied => {
            run.refund(f, seq, a.id)?;
            return Ok(Some(AttemptFlow::Continue));
        }
        Environment::Ask(reason) => return Ok(Some(AttemptFlow::Exit(blocked_on(reason)))),
        Environment::Left => {}
    }
    if reask_reproduction(f, step, a, verdict, run, seq, &mut state.feedback)? {
        return Ok(Some(AttemptFlow::Continue));
    }
    if let Some(flow) = rewind_tests(args, a, verdict).await? {
        return Ok(Some(AttemptFlow::Exit(flow)));
    }
    Ok(None)
}

/// Consecutive provider refusals a directive tolerates before giving up the
/// slot rather than relaunching forever on a provider that never relents.
const REFUSAL_LIMIT: u32 = 5;

/// A provider kept refusing past `REFUSAL_LIMIT`: the task fails without
/// spending an attempt, since none of the refusals counted as one.
fn refusal_exhausted() -> End {
    End::Failed {
        reason: format!("the provider refused {REFUSAL_LIMIT} times in a row without a window"),
        counted: false,
        pushes: false,
    }
}

/// A review whose demotion cited files only its sandbox had is asked,
/// once, to inline the reproduction (`verify::review::reask`): that
/// attempt does not count against the step, and the ask becomes the
/// feedback the next attempt is shown. True when it asked.
fn reask_reproduction(
    f: &Forge,
    step: &workflows::ResolvedStep,
    a: &crate::store::Attempt,
    verdict: &verify::Verdict,
    run: &mut Run,
    seq: i64,
    feedback: &mut Option<String>,
) -> Result<bool, Fault> {
    if step.action.contract != Contract::Review || a.state != AttemptState::ChecksFailed {
        return Ok(false);
    }
    let Some(ask) = verify::review::reask(&verdict.checks, feedback.as_deref(), a.task_id) else {
        return Ok(false);
    };
    f.report.emit(
        a.task_id,
        Event::Note {
            text: "review   the demotion cites files outside the clone; asking the reviewer once to inline its reproduction",
        },
    );
    run.refund(f, seq, a.id)?;
    *feedback = Some(ask);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{Fixture, check, verdict};
    use super::*;

    #[tokio::test]
    async fn namespace_failure_refunds_persists_rewinds_and_resets_the_coder() {
        let mut x = Fixture::new();
        let repo = PathBuf::from(&x.t.worktree);
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("code"), "base").unwrap();
        x.t.base_sha = git::init_commit_all(&repo, "base").await.unwrap();
        std::fs::write(repo.join("code"), "attempt").unwrap();
        git::commit_all(&repo, "attempt").await.unwrap();
        let a = x.attempt(AttemptState::ChecksFailed);
        let mut v = verdict();
        v.checks.push(check("tests/acceptance/check.ts:3:1 error"));
        let flow = rewind_tests(&mut x.args(), &a, &v).await.ok().unwrap();
        assert!(matches!(flow, Some(StepFlow::Again)));
        assert_eq!(x.run.used_at(2), 0);
        assert_eq!(x.run.used_at(1), 1);
        assert!(x.f.store.refunded_attempts(x.t.id).unwrap().contains(&a.id));
        assert_eq!(x.run.idx, 0);
        assert!(x.run.done.is_empty());
        assert!(x.run.owed[&1].contains("tests/acceptance/check.ts"));
        assert_eq!(git::head(&repo).await.unwrap(), x.t.base_sha);
    }

    #[tokio::test]
    async fn exhausted_tests_fail_without_refunding_or_rewinding() {
        let mut x = Fixture::new();
        x.run.used.insert(1, x.t.max_attempts);
        let a = x.attempt(AttemptState::ChecksFailed);
        let mut v = verdict();
        v.checks.push(check("tests/acceptance/check.ts:3:1 error"));
        let flow = rewind_tests(&mut x.args(), &a, &v).await.ok().unwrap();
        assert!(
            matches!(flow, Some(StepFlow::End(End::Failed { counted: true, pushes: false, reason })) if reason.contains("after 2 tests attempt(s)"))
        );
        assert_eq!(x.run.idx, 1);
        assert_eq!(x.run.used_at(2), 1);
        assert!(x.f.store.refunded_attempts(x.t.id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn unrelated_failures_and_tests_own_failures_do_not_rewind() {
        let mut x = Fixture::new();
        let mut a = x.attempt(AttemptState::ChecksFailed);
        let mut v = verdict();
        for (state, contract, tail) in [
            (
                AttemptState::AgentFailed,
                Contract::Code,
                "tests/acceptance/a.ts:3 error",
            ),
            (
                AttemptState::ChecksFailed,
                Contract::Tests,
                "tests/acceptance/a.ts:3 error",
            ),
            (
                AttemptState::ChecksFailed,
                Contract::Code,
                "src/a.ts:3 error",
            ),
        ] {
            a.state = state;
            x.resolved.steps[1].action.contract = contract;
            v.checks = vec![check(tail)];
            assert!(
                rewind_tests(&mut x.args(), &a, &v)
                    .await
                    .ok()
                    .unwrap()
                    .is_none()
            );
            assert_eq!(x.run.idx, 1);
            assert_eq!(x.run.used_at(2), 1);
        }
    }

    #[tokio::test]
    async fn refusals_preserve_session_and_feedback_and_stop_at_the_limit() {
        let mut x = Fixture::new();
        let a = x.attempt(AttemptState::AgentFailed);
        let ts = x.t.clone();
        let v = verdict();
        let outcome = crate::agent::Outcome {
            rate_limited: true,
            ..Default::default()
        };
        let mut state = AttemptLoop {
            feedback: Some("fix this".into()),
            resume: Some(Resume {
                session: "session".into(),
                start_sha: "base".into(),
                fresh_from: None,
            }),
            ..Default::default()
        };
        for _ in 0..REFUSAL_LIMIT {
            x.run.used.insert(2, 1);
            let flow = retry_after_attempt(&mut x.args(), &mut state, &ts, &a, &v, &outcome)
                .await
                .ok()
                .unwrap();
            assert!(matches!(flow, Some(AttemptFlow::Continue)));
            assert_eq!(x.run.used_at(2), 0);
            assert_eq!(state.feedback.as_deref(), Some("fix this"));
            assert_eq!(state.resume.as_ref().unwrap().session, "session");
        }
        x.run.used.insert(2, 1);
        let flow = retry_after_attempt(&mut x.args(), &mut state, &ts, &a, &v, &outcome)
            .await
            .ok()
            .unwrap();
        assert!(matches!(
            flow,
            Some(AttemptFlow::Exit(StepFlow::End(End::Failed {
                counted: false,
                pushes: false,
                ..
            })))
        ));
        // The existing limit check occurs before the final refund.
        assert_eq!(x.run.used_at(2), 1);
    }

    #[tokio::test]
    async fn unverified_refusal_falls_through_and_resets_the_streak() {
        let mut x = Fixture::new();
        let a = x.attempt(AttemptState::Unverified);
        let ts = x.t.clone();
        let mut state = AttemptLoop {
            consecutive_refusals: 4,
            ..Default::default()
        };
        let outcome = crate::agent::Outcome {
            rate_limited: true,
            ..Default::default()
        };
        assert!(
            retry_after_attempt(&mut x.args(), &mut state, &ts, &a, &verdict(), &outcome)
                .await
                .ok()
                .unwrap()
                .is_none()
        );
        assert_eq!(state.consecutive_refusals, 0);
        assert_eq!(x.run.used_at(2), 1);
    }
}
