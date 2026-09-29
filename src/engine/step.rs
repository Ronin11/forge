//! Running one step of the resolved workflow: an operation (deterministic,
//! one shot, with its own rewind-on-failure rules) or a directive (attempted
//! until verified or the budget is spent, every retry told what failed).

use super::*;

/// Workflow state and completed operations needed to execute an operation step.
pub(super) struct RunOperationStep<'a> {
    pub(super) f: &'a Forge,
    pub(super) t: &'a mut Task,
    pub(super) cfg: &'a config::Config,
    pub(super) resolved: &'a workflows::Resolved,
    pub(super) run: &'a mut Run,
    pub(super) step: &'a workflows::ResolvedStep,
    pub(super) seq: i64,
    pub(super) prior_ops: &'a [Op],
    pub(super) done_ops: &'a HashSet<i64>,
    pub(super) merged_base_retry: bool,
}

/// Workflow and retry state needed to execute a directive step.
pub(super) struct RunDirectiveStep<'a> {
    pub(super) f: &'a Forge,
    pub(super) t: &'a mut Task,
    pub(super) cfg: &'a config::Config,
    pub(super) resolved: &'a workflows::Resolved,
    pub(super) run: &'a mut Run,
    pub(super) step: &'a workflows::ResolvedStep,
    pub(super) seq: i64,
    pub(super) attempt_no: &'a mut i64,
    pub(super) task_cap: f64,
    pub(super) repo: &'a Path,
    pub(super) wt: &'a Path,
    pub(super) wait: bool,
    pub(super) remote_url: &'a Option<String>,
}

/// The run ends blocked on a question for the operator.
pub(super) fn blocked_on(reason: String) -> StepFlow {
    StepFlow::End(End::Blocked {
        reason,
        demoted: false,
        to: None,
    })
}

/// What one step of the run decided: move to the next step, go round
/// again from wherever the cursor now points (a rewind), end the run, or
/// give up the slot: the step's own provider is held, so the task goes
/// back to `queued` with the hold as its reason rather than sleep out the
/// window inside this attempt (the claim loop's own hold logic waits for
/// the reset).
pub(super) enum StepFlow {
    Next,
    Again,
    End(End),
    Requeue(String),
}

/// One operation step of the run: skipped when an earlier worker already
/// ran and verified it, run otherwise. A verifying operation that fails
/// sends the run back to the directive it judges, within its attempts;
/// `setup` failing after a merged-base retry hands the coder the error
/// instead of failing the task; any other failure ends the run.
pub(super) async fn run_operation_step(args: RunOperationStep<'_>) -> Result<StepFlow, Fault> {
    let RunOperationStep {
        f,
        t,
        cfg,
        resolved,
        run,
        step,
        seq,
        prior_ops,
        done_ops,
        merged_base_retry,
    } = args;
    // A mutating operation counts as done only once the kernel
    // verified what it committed; a worker that died in between
    // runs it again, which is harmless: it is deterministic and
    // a second commit finds nothing to commit. A verifying
    // operation always runs again after the directive it judges.
    let verified_here = prior_ops
        .iter()
        .any(|o| o.kernel && o.name == "verify" && o.seq == seq && o.ok);
    if done_ops.contains(&seq) && (!step.action.mutates() || verified_here) && !step.action.verifies
    {
        return Ok(StepFlow::Next);
    }
    let (mut ok, mut detail) = run_operation(f, t, cfg, step, seq).await?;
    // A need the [environment] policy covers is granted and the step runs
    // again, no question and nothing counted; each grant applies once.
    while !ok {
        match apply_environment(f, t, cfg, &detail).await? {
            Environment::Applied => (ok, detail) = run_operation(f, t, cfg, step, seq).await?,
            Environment::Ask(reason) => return Ok(blocked_on(reason)),
            Environment::Left => break,
        }
    }
    if ok {
        return Ok(StepFlow::Next);
    }
    // A merge that is textually clean can still be
    // semantically broken; that surfaces here, at setup,
    // before any attempt. It is the merge's problem, not
    // the task's: hand the coder the error on the verified
    // branch instead of failing with nothing to show for it.
    if merged_base_retry
        && step.action.name == "setup"
        && let Some(c_idx) = resolved.steps[run.idx..]
            .iter()
            .position(|s| s.action.kind == Kind::Directive && s.action.contract == Contract::Code)
            .map(|i| run.idx + i)
    {
        let c_name = resolved.steps[c_idx].action.name.clone();
        f.report.emit(
            t.id,
            Event::Note {
                text: &format!(
                    "setup    the merged base does not build; {c_name} will see the error"
                ),
            },
        );
        run.owed.insert(
            c_idx as i64 + 1,
            format!(
                "Setup failed after merging the current base into this verified branch:\n{}\nThe merge is textually clean but the result does not build. Fix it, leave the tree clean, and commit.",
                crate::checks::last_lines(&detail, 30)
            ),
        );
        return Ok(StepFlow::Next);
    }
    if step.action.verifies
        && let Some(d_idx) = (0..run.idx)
            .rev()
            .find(|&i| resolved.steps[i].action.kind == Kind::Directive)
    {
        let d_seq = d_idx as i64 + 1;
        if run.used_at(d_seq) < t.max_attempts {
            let d_name = resolved.steps[d_idx].action.name.clone();
            f.report.emit(
                t.id,
                Event::Note {
                    text: &format!(
                        "verify   {} failed; back to {} for another attempt",
                        step.action.name, d_name
                    ),
                },
            );
            run.rewind(d_idx, format!("The `{}` verification failed after your change:\n{}\nFix it, leave the tree clean, and commit.", step.action.name, detail));
            return Ok(StepFlow::Again);
        }
        return Ok(StepFlow::End(End::Failed {
            reason: format!(
                "operation {} (verifies) failed after {} attempt(s): {}",
                step.action.name,
                run.used_at(d_seq),
                detail.lines().next().unwrap_or("")
            ),
            counted: false,
            pushes: false,
        }));
    }
    Ok(StepFlow::End(End::Failed {
        // `setup` gets the full tail: a build failure on a
        // fresh clone means the repository or the base is
        // broken, and `forge show` needs more than the
        // first line of a compiler's output to say why.
        reason: if step.action.name == "setup" {
            format!(
                "operation setup failed:\n{}",
                crate::checks::last_lines(&detail, 30)
            )
        } else {
            format!(
                "operation {} failed: {}",
                step.action.name,
                detail.lines().next().unwrap_or("")
            )
        },
        counted: false,
        pushes: false,
    }))
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

/// One directive step of the run: attempts until one verifies or the
/// directive is out of them, with the window hold, the budget, the resume
/// of a capped session, the tests-fault rewind, and what each contract
/// records on success (the interface, the plan, a filed initiative). When
/// the directive stops short, what that means for the task: a capped
/// coder's clean commit goes to a human if the checks pass, a reviewer
/// that never ruled leaves the branch unverified, a question blocks.
pub(super) async fn run_directive_step(args: RunDirectiveStep<'_>) -> Result<StepFlow, Fault> {
    let RunDirectiveStep {
        f,
        t,
        cfg,
        resolved,
        run,
        step,
        seq,
        attempt_no,
        task_cap,
        repo,
        wt,
        remote_url,
        wait,
    } = args;
    let id = t.id;
    if run.done.contains(&seq) && !run.owed.contains_key(&seq) {
        f.report.emit(
            id,
            Event::Note {
                text: &format!("step     {} already verified; resuming", step.action.name),
            },
        );
        return Ok(StepFlow::Next);
    }
    // Per-step parameters (the workflow's override, else the action's
    // default, else the task's), the provider this step's role runs
    // under, and the routing record for it.
    let role = step.action.contract.as_str();
    let mut feedback: Option<String> = run.owed.remove(&seq);
    let mut resume: Option<Resume> = None;
    let mut step_ok = false;
    // How the directive's last attempt ended, for the step's End.
    let mut last = AttemptState::Running;
    let mut last_reason = String::new();
    // Who a blocking question is addressed to, from the
    // envelope's `needs_input.to`; `None` means the operator.
    let mut last_to: Option<String> = None;
    // The last attempt's own rows, so the reason built after
    // the loop can name the L0 rules that actually failed
    // rather than rely on `last_reason` alone.
    let mut last_checks: Vec<CheckResult> = Vec::new();
    // The last attempt ran out of turns after committing, tree
    // clean, no result: the checks can still judge the code.
    let mut capped_committed = false;
    let mut consecutive_refusals = 0u32;
    // A placeholder `needs_input` question (see
    // `envelope::is_placeholder_question`) gets exactly one nudge to
    // finish rather than blocking the operator on it: `nudged` stops a
    // second one from getting the same treatment, and `nudge_pending`
    // grants the round that spends it even on what would otherwise be
    // the directive's last attempt (task 1069, 2026-09-28: a one-letter
    // question after 49 turns and 3 commits sat blocked for want of
    // exactly this).
    let mut nudged = false;
    let mut nudge_pending = false;
    while run.used_at(seq) < t.max_attempts || nudge_pending {
        nudge_pending = false;
        if let Some(flow) = super::provider_hold::before_attempt(f, t, role, wait).await? {
            if let Some(fb) = feedback {
                run.owed.insert(seq, fb);
            }
            return Ok(flow);
        }
        let ts = per_step_task(f, t, step, role)?;
        // Would the next attempt cross the cap? A decision for a human,
        // not a failure: see `check_cap`.
        if let Some(end) = check_cap(f, t, resolved, &run.done, task_cap, wt).await? {
            return Ok(StepFlow::End(end));
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
            feedback: feedback.as_deref(),
            resume: resume.as_ref(),
            cursor: Some(run.cursor(t, *attempt_no).after(*attempt_no)),
        })
        .await?;
        // `forge withdraw --abort` stopped this attempt (see
        // `attempt::launch`): the task ends here, not on its verdict.
        if let Some(end) = check_abort(f, t)? {
            return Ok(StepFlow::End(end));
        }
        // A deterministic fix ran before this verdict was
        // decided (see `verify::try_known_fix`): its own
        // row, so the trace shows what Forge did without an
        // agent turn before showing whether it worked.
        if let Some(fix) = &verdict.known_fix {
            op(
                f,
                id,
                &timer,
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
            &timer,
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
        last = a.state;
        last_reason = a.reason.clone();
        last_checks = verdict.checks.clone();
        last_to = verdict
            .envelope
            .as_ref()
            .and_then(|e| e.needs_input.as_ref())
            .and_then(|q| crate::envelope::addressee(q.to.as_deref()));
        // The provider refused the run: not an attempt the agent
        // spent. The hold at the top of the loop waits for the
        // window, or for a refused login to answer a probe; the same
        // feedback and session go again.
        if outcome.rate_limited && a.state != AttemptState::Unverified {
            if outcome.login_refused {
                crate::login_hold::hold(f, &ts.provider, &outcome, id).env()?;
            }
            consecutive_refusals += 1;
            if consecutive_refusals > REFUSAL_LIMIT {
                return Ok(StepFlow::End(refusal_exhausted()));
            }
            f.report.emit(
                id,
                Event::Note {
                    text: "rate     the provider refused this run; it does not count as an attempt",
                },
            );
            run.refund(f, seq, a.id)?;
            continue;
        }
        consecutive_refusals = 0;
        // An environment need the policy covers (a refused host, a host cache) is
        // applied and rerun without counting; anything else falls through.
        match environment_after(f, t, cfg, &a, &verdict).await? {
            Environment::Applied => {
                run.refund(f, seq, a.id)?;
                continue;
            }
            Environment::Ask(reason) => return Ok(blocked_on(reason)),
            Environment::Left => {}
        }
        if reask_reproduction(f, step, &a, &verdict, run, seq, &mut feedback)? {
            continue;
        }
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
                return Ok(StepFlow::Again);
            }
            return Ok(StepFlow::End(End::Failed {
                reason: format!(
                    "check {check} failed inside the verification namespace after {t_used} tests attempt(s): {}",
                    tail.lines().next().unwrap_or("")
                ),
                counted: true,
                pushes: false,
            }));
        }
        match a.state {
            AttemptState::Succeeded => {
                if step.action.contract == Contract::Tests {
                    let tests_dir = tests_clone_dir(&t.worktree);
                    git::push_to_repo(&f.paths.home, repo, &tests_dir, &format!("verify/{}", t.id))
                        .await
                        .task()?;
                    if let Some(url) = &remote_url
                        && let Err(e) = git::push(
                            &f.paths.home,
                            repo,
                            &tests_dir,
                            url,
                            &format!("verify/{}", t.id),
                        )
                        .await
                    {
                        f.report.emit(
                            id,
                            Event::Note {
                                text: &format!("tests    push of verify/{} failed: {e:#}", t.id),
                            },
                        );
                    }
                    t.interface = verdict
                        .envelope
                        .as_ref()
                        .map(|e| e.summary.clone())
                        .unwrap_or_default();
                    f.store.update_task(t).env()?;
                }
                if step.action.contract == Contract::Plan {
                    // The plan is the product: shown to every later
                    // directive, verified only to name real paths.
                    t.plan = verdict
                        .envelope
                        .as_ref()
                        .map(|e| e.summary.clone())
                        .unwrap_or_default();
                    f.store.update_task(t).env()?;
                    f.report.emit(
                        id,
                        Event::Note {
                            text: &format!(
                                "plan     {} line(s) from {}",
                                t.plan.lines().count(),
                                step.action.name
                            ),
                        },
                    );
                    // file_into_initiative: the plan's items become
                    // sibling tasks in the same initiative instead
                    // of this task running the code step itself.
                    if step.action.file_into_initiative
                        && let Some(iid) = t.initiative
                    {
                        let filed = crate::queue::file_plan(f, t, iid).await.task()?;
                        f.report.emit(
                            id,
                            Event::Note {
                                text: &format!(
                                    "filed    {} task(s) into initiative {iid}: {}",
                                    filed.len(),
                                    filed
                                        .iter()
                                        .map(i64::to_string)
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                            },
                        );
                        return Ok(StepFlow::End(End::Filed {
                            n: filed.len(),
                            initiative: iid,
                            last: *filed.last().unwrap_or(&id),
                        }));
                    }
                }
                step_ok = true;
                break;
            }
            AttemptState::NeedsInput => {
                // A placeholder question is the agent stopping early with
                // nothing left to ask, not a question for the operator:
                // one nudge, in the same session, before it can block.
                if !nudged
                    && let Some((r, fb)) = nudge_placeholder_question(t, &a, &verdict, &outcome)
                {
                    nudged = true;
                    nudge_pending = true;
                    f.report.emit(
                        id,
                        Event::Note {
                            text: "resume   the question was not really one; nudging the session to finish instead of blocking",
                        },
                    );
                    resume = Some(r);
                    feedback = Some(fb);
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
                    break;
                }
            }
            AttemptState::Unverified => break,
            AttemptState::ChecksFailed | AttemptState::AgentFailed => {
                // Out of turns before producing a result: continue the
                // same session rather than start over blind. Even with
                // nothing on the tree, the session holds what the agent
                // located; a fresh attempt would spend its turns finding
                // it again (33 of the first 214 attempts did exactly
                // that). A capped attempt that did return a result gets
                // the ordinary feedback for what its result failed.
                let capped = outcome.max_turns_hit || outcome.num_turns >= ts.max_turns;
                let stopped = outcome.ended_early.is_some();
                let unfinished = verdict.envelope.is_none();
                let progress = verdict.commits > 0 || verdict.dirty;
                capped_committed = capped && unfinished && verdict.commits > 0 && !verdict.dirty;
                let over = fresh_arm(t)
                    && std::fs::read_to_string(&a.log_path)
                        .ok()
                        .and_then(|l| crate::handoff::last_context_tokens(&l))
                        .is_some_and(|n| n > crate::handoff::CONTEXT_THRESHOLD_TOKENS);
                if (capped || stopped || over)
                    && unfinished
                    && let Some(sid) = &outcome.session_id
                {
                    f.report.emit(
                        id,
                        Event::Note {
                            text: &format!(
                                "resume   continuing session {} {}",
                                &sid[..sid.len().min(8)],
                                if stopped {
                                    "after stopping it early"
                                } else if capped {
                                    "past the turn cap"
                                } else {
                                    "past the context threshold"
                                }
                            ),
                        },
                    );
                    resume = Some(Resume {
                        session: sid.clone(),
                        start_sha: a.start_sha.clone(),
                        fresh_from: fresh_arm(t).then(|| PathBuf::from(&a.log_path)),
                    });
                    feedback = Some(if let Some(why) = &outcome.ended_early {
                        early_feedback(why, &outcome.early_signals)
                    } else if progress {
                        "You ran out of turns before finishing. Continue exactly where you left off: finish the work, leave the tree clean, commit, and return the structured result.".to_string()
                    } else {
                        "You ran out of turns before changing anything. You have already read what you need: stop exploring, make the change now, commit as soon as it compiles, and return the structured result.".to_string()
                    });
                } else if t.resume_on_failure
                    && a.state == AttemptState::ChecksFailed
                    && let Some(sid) = &outcome.session_id
                {
                    // The operator asked to keep going in the same
                    // session after a failed attempt, not just a
                    // capped one: same feedback, same CLI session.
                    f.report.emit(
                        id,
                        Event::Note {
                            text: &format!(
                                "resume   continuing session {} after failed checks",
                                &sid[..sid.len().min(8)]
                            ),
                        },
                    );
                    resume = Some(Resume {
                        session: sid.clone(),
                        start_sha: a.start_sha.clone(),
                        fresh_from: over.then(|| PathBuf::from(&a.log_path)),
                    });
                    feedback = Some(verify::feedback(&verdict, &outcome, ts.max_turns));
                } else {
                    resume = None;
                    feedback = Some(verify::feedback(&verdict, &outcome, ts.max_turns));
                }
            }
            AttemptState::Running => {
                unreachable!("attempt returned in running state")
            }
        }
    }
    if step_ok {
        run.done.insert(seq);
        return Ok(StepFlow::Next);
    }
    // The directive is out of attempts, or stopped: what that
    // means for the task.
    // A coder that ran out of turns after committing a clean
    // tree left code the checks can judge. If they pass, no
    // agent vouched for it, so it goes to a human as unverified
    // rather than being thrown away.
    if step.action.contract == Contract::Code
        && last == AttemptState::AgentFailed
        && capped_committed
    {
        let overlay = overlay_refs(repo, t.id, Some(&t.verify_base)).await;
        let v = verify::verify_integration(&Subject {
            task_id: t.id,
            repo,
            worktree: wt,
            base_sha: &t.base_sha,
            start_sha: &t.base_sha,
            branch: &t.branch,
            cfg,
            task_checks: &t.checks,
            paths: &[],
            allow_protected: t.allow_protected,
            overlay_refs: &overlay,
            pending_main: None,
            sandbox: f.sandbox.as_ref(),
            report: &f.report,
            logs_dir: &f.paths.logs,
            scratch: None,
            plan_rows: true,
        })
        .await
        .task()?;
        let reason = if v.state == AttemptState::Succeeded {
            "ran out of turns after committing; the checks pass but no result was returned, so the branch goes to a human".to_string()
        } else {
            format!(
                "ran out of turns after committing; the checks fail: {}",
                v.reason
            )
        };
        f.report.emit(
            id,
            Event::Note {
                text: &format!("capped   {reason}"),
            },
        );
        return Ok(StepFlow::End(if v.state == AttemptState::Succeeded {
            End::Unverified(reason)
        } else {
            End::Failed {
                reason,
                counted: true,
                pushes: false,
            }
        }));
    }
    // A reviewer that never reached a verdict is not evidence
    // of a defect: the branch verified at the code step, so it
    // goes to a human as unverified instead of failing.
    if step.action.contract == Contract::Review && last == AttemptState::AgentFailed {
        return Ok(StepFlow::End(End::Unverified(format!(
            "review could not finish ({last_reason}); the branch verified at the code step and goes to human review"
        ))));
    }
    Ok(StepFlow::End(match last {
        AttemptState::NeedsInput => End::Blocked {
            reason: last_reason.clone(),
            demoted: last_reason.starts_with("review demoted"),
            to: last_to.clone(),
        },
        AttemptState::Unverified => End::Unverified(last_reason.clone()),
        _ => End::Failed {
            reason: l0_failure_reason(&last_checks).unwrap_or_else(|| last_reason.clone()),
            counted: true,
            pushes: false,
        },
    }))
}

/// Per-step task parameters (the workflow's override, else the action's
/// default, else the task's), the provider this step's role runs under
/// (the task's own flag, else its project's `[roles]`, else the
/// operator's, else "anthropic" — see `ctx::resolve_provider`), and the
/// routing record for `role` (docs/ECONOMIST.md, "The routing record"):
/// written before the first attempt spends anything, so even a step that
/// never verifies still shows its own routing.
fn per_step_task(
    f: &Forge,
    t: &mut Task,
    step: &workflows::ResolvedStep,
    role: &str,
) -> Result<Task, Fault> {
    let mut ts = t.clone();
    if let Some(m) = &step.model {
        ts.model = m.clone();
        ts.model_source = "step".to_string();
    }
    if let Some(n) = step.max_turns {
        ts.max_turns = n as i64;
    }
    if let Some(n) = step.timeout_secs {
        ts.timeout_secs = n as i64;
    }
    let (provider, provider_source) = f.effective_provider_routed(&ts, role).env()?;
    ts.provider = provider.name.clone();
    t.routing.insert(
        role.to_string(),
        crate::store::RoleRouting {
            provider: crate::store::Routed {
                value: ts.provider.clone(),
                source: provider_source.to_string(),
            },
            model: crate::store::Routed {
                value: crate::attempt::attempt_model(
                    &step.action.name,
                    &ts.model,
                    &ts.model,
                    provider,
                    crate::attempt::model_pinned(&ts.model_source),
                ),
                source: crate::attempt::attempt_model_source(
                    step.model.as_deref(),
                    provider,
                    &t.model_source,
                ),
            },
            workflow: crate::store::Routed {
                value: t.workflow.clone(),
                source: t.workflow_source.clone(),
            },
        },
    );
    f.store.update_task(t).env()?;
    Ok(ts)
}

/// Whether a `needs_input` question is a placeholder
/// (`envelope::is_placeholder_question`) with a session to continue: if
/// so, the resume and the feedback that ask it to finish instead of
/// asking again.
fn nudge_placeholder_question(
    t: &Task,
    a: &crate::store::Attempt,
    verdict: &verify::Verdict,
    outcome: &crate::agent::Outcome,
) -> Option<(Resume, String)> {
    let question = verdict
        .envelope
        .as_ref()
        .and_then(|e| e.needs_input.as_ref())
        .map(|q| q.question.as_str())
        .unwrap_or("");
    if !crate::envelope::is_placeholder_question(question) {
        return None;
    }
    let sid = outcome.session_id.as_ref()?;
    Some((
        Resume {
            session: sid.clone(),
            start_sha: a.start_sha.clone(),
            fresh_from: fresh_arm(t).then(|| PathBuf::from(&a.log_path)),
        },
        "there is no open question; finish the task and return a result".to_string(),
    ))
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

/// Whether the task drew the `fresh` arm of the continuation factor
/// (docs/CONTEXT.md): a continuation starts a new session on a handoff.
fn fresh_arm(t: &Task) -> bool {
    t.explore.get("continuation").map(String::as_str) == Some("fresh")
}
