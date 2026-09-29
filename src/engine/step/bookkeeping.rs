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
