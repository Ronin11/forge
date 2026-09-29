//! The attempts within one directive and their recorded verification rows.
use super::*;

pub(super) fn record_verification(
    f: &Forge,
    id: i64,
    seq: i64,
    timer: &Timer,
    a: &crate::store::Attempt,
    verdict: &verify::Verdict,
) -> Result<(), Fault> {
    // A deterministic fix ran before this verdict was
    // decided (see `verify::try_known_fix`): its own
    // row, so the trace shows what Forge did without an
    // agent turn before showing whether it worked.
    if let Some(fix) = &verdict.known_fix {
        op(
            f,
            id,
            timer,
            OpRow {
                seq,
                name: "known-fix",
                kernel: true,
                ok: fix.ok,
                exit: None,
                detail: &match &fix.commit {
                    Some(sha) => format!(
                        "{} fixed as {}: {}",
                        fix.checks.join(", "),
                        &sha[..8],
                        fix.diff_stat
                    ),
                    None => format!("{} left nothing to commit", fix.checks.join(", ")),
                },
                attempt_id: Some(a.id),
                output: "",
            },
        )?;
    }
    // The kernel's verify, as a row of its own.
    op(
        f,
        id,
        timer,
        OpRow {
            seq,
            name: "verify",
            kernel: true,
            ok: a.state == AttemptState::Succeeded,
            exit: None,
            detail: &a.reason,
            attempt_id: Some(a.id),
            output: "",
        },
    )?;
    Ok(())
}

/// Session, retry allowances, and last-result evidence carried between attempts.
#[derive(Default)]
pub(super) struct AttemptLoop {
    pub(super) feedback: Option<String>,
    pub(super) resume: Option<Resume>,
    pub(super) step_ok: bool,
    pub(super) last: AttemptState,
    pub(super) last_reason: String,
    pub(super) last_to: Option<String>,
    pub(super) last_checks: Vec<CheckResult>,
    pub(super) capped_committed: bool,
    pub(super) consecutive_refusals: u32,
    // A placeholder question gets one nudge, including beyond the attempt budget.
    pub(super) nudged: bool,
    pub(super) nudge_pending: bool,
}

enum AttemptFlow {
    Continue,
    Stop,
    Exit(StepFlow),
}

pub(super) async fn run_attempt_loop(
    args: &mut RunDirectiveStep<'_>,
    state: &mut AttemptLoop,
) -> Result<Option<StepFlow>, Fault> {
    while args.run.used_at(args.seq) < args.t.max_attempts || state.nudge_pending {
        let f = args.f;
        let t = &mut *args.t;
        let cfg = args.cfg;
        let resolved = args.resolved;
        let run = &mut *args.run;
        let step = args.step;
        let seq = args.seq;
        let attempt_no = &mut *args.attempt_no;
        let task_cap = args.task_cap;
        let wt = args.wt;
        let wait = args.wait;
        let id = t.id;
        let role = step.action.contract.as_str();
        state.nudge_pending = false;
        if let Some(flow) = provider_hold::before_attempt(f, t, role, wait).await? {
            if let Some(fb) = state.feedback.take() {
                run.owed.insert(seq, fb);
            }
            return Ok(Some(flow));
        }
        let ts = per_step_task(f, t, step, role)?;
        // Would the next attempt cross the cap? A decision for a human,
        // not a failure: see `check_cap`.
        if let Some(end) = check_cap(f, t, resolved, &run.done, task_cap, wt).await? {
            return Ok(Some(StepFlow::End(end)));
        }
        *run.used.entry(seq).or_insert(0) += 1;
        let n = run.used[&seq];
        *attempt_no += 1;
        f.report.emit(
            id,
            Event::AttemptStarted {
                n,
                of: t.max_attempts,
            },
        );
        f.report.emit(
            id,
            Event::Note {
                text: &format!("step     {} ({})", step.action.name, step.via.join(" → ")),
            },
        );
        let timer = Timer::now();
        let (a, verdict, outcome) = crate::attempt::run_attempt(crate::attempt::RunAttempt {
            f,
            t: &ts,
            cfg,
            step,
            seq,
            attempt_no: *attempt_no,
            feedback: state.feedback.as_deref(),
            resume: state.resume.as_ref(),
            cursor: Some(run.cursor(t, *attempt_no).after(*attempt_no)),
        })
        .await?;
        // `forge withdraw --abort` stopped this attempt (see
        // `attempt::launch`): the task ends here, not on its verdict.
        if let Some(end) = check_abort(f, t)? {
            return Ok(Some(StepFlow::End(end)));
        }
        attempts::record_verification(f, id, seq, &timer, &a, &verdict)?;
        state.last = a.state;
        state.last_reason = a.reason.clone();
        state.last_checks = verdict.checks.clone();
        state.last_to = verdict
            .envelope
            .as_ref()
            .and_then(|e| e.needs_input.as_ref())
            .and_then(|q| crate::envelope::addressee(q.to.as_deref()));
        let flow = match retry_after_attempt(args, state, &ts, &a, &verdict, &outcome).await? {
            Some(flow) => flow,
            None => settle_attempt(args, state, &ts, &a, &verdict, &outcome).await?,
        };
        match flow {
            AttemptFlow::Continue => {}
            AttemptFlow::Stop => break,
            AttemptFlow::Exit(flow) => return Ok(Some(flow)),
        }
    }
    Ok(None)
}

async fn retry_after_attempt(
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
    // state.feedback and session go again.
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
    if let Some(flow) = bookkeeping::rewind_tests(args, a, verdict).await? {
        return Ok(Some(AttemptFlow::Exit(flow)));
    }
    Ok(None)
}

async fn settle_attempt(
    args: &mut RunDirectiveStep<'_>,
    state: &mut AttemptLoop,
    ts: &Task,
    a: &crate::store::Attempt,
    verdict: &verify::Verdict,
    outcome: &crate::agent::Outcome,
) -> Result<AttemptFlow, Fault> {
    let f = args.f;
    let t = &mut *args.t;
    let step = args.step;
    let repo = args.repo;
    let remote_url = args.remote_url;
    let id = t.id;
    match a.state {
        AttemptState::Succeeded => {
            if let Some(flow) =
                success::record_success(f, t, step, repo, remote_url, verdict).await?
            {
                return Ok(AttemptFlow::Exit(flow));
            }
            state.step_ok = true;
            return Ok(AttemptFlow::Stop);
        }
        AttemptState::NeedsInput => {
            // A placeholder question is the agent stopping early with
            // nothing left to ask, not a question for the operator:
            // one nudge, in the same session, before it can block.
            if !state.nudged
                && let Some((r, fb)) = nudge_placeholder_question(t, a, verdict, outcome)
            {
                state.nudged = true;
                state.nudge_pending = true;
                f.report.emit(
                    id,
                    Event::Note {
                        text: "resume   the question was not really one; nudging the session to finish instead of blocking",
                    },
                );
                state.resume = Some(r);
                state.feedback = Some(fb);
            } else {
                // The interview's confirmation turn writes
                // the brief alongside the question that
                // asks the person to confirm it; every
                // other contract's question stops here
                // with nothing recorded as a plan.
                if step.action.name == "interview"
                    && let Some(summary) = verdict
                        .envelope
                        .as_ref()
                        .map(|e| e.summary.trim().to_string())
                    && !summary.is_empty()
                {
                    t.plan = summary;
                    f.store.update_task(t).env()?;
                }
                return Ok(AttemptFlow::Stop);
            }
        }
        AttemptState::Unverified => return Ok(AttemptFlow::Stop),
        AttemptState::ChecksFailed | AttemptState::AgentFailed => {
            (state.resume, state.feedback, state.capped_committed) =
                continuation::after_failure(f, t, ts, a, verdict, outcome);
        }
        AttemptState::Running => {
            unreachable!("attempt returned in running state")
        }
    }
    Ok(AttemptFlow::Continue)
}
