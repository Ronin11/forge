//! Running one step of the resolved workflow: an operation (deterministic,
//! one shot, with its own rewind-on-failure rules) or a directive (attempted
//! until verified or the budget is spent, every retry told what failed).

use super::*;

mod attempts;
mod bookkeeping;
mod continuation;
mod provider_hold;
mod success;

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

/// One directive step of the run: attempts until one verifies or the
/// directive is out of them, with the window hold, the budget, the resume
/// of a capped session, the tests-fault rewind, and what each contract
/// records on success (the interface, the plan, a filed initiative). When
/// the directive stops short, what that means for the task: a capped
/// coder's clean commit goes to a human if the checks pass, a reviewer
/// that never ruled leaves the branch unverified, a question blocks.
pub(super) async fn run_directive_step(mut args: RunDirectiveStep<'_>) -> Result<StepFlow, Fault> {
    if args.run.done.contains(&args.seq) && !args.run.owed.contains_key(&args.seq) {
        args.f.report.emit(
            args.t.id,
            Event::Note {
                text: &format!(
                    "step     {} already verified; resuming",
                    args.step.action.name
                ),
            },
        );
        return Ok(StepFlow::Next);
    }
    let mut state = attempts::AttemptLoop {
        feedback: args.run.owed.remove(&args.seq),
        ..Default::default()
    };
    if let Some(flow) = attempts::run_attempt_loop(&mut args, &mut state).await? {
        return Ok(flow);
    }
    let RunDirectiveStep {
        f,
        t,
        cfg,
        run,
        step,
        seq,
        repo,
        wt,
        ..
    } = args;
    let id = t.id;
    let attempts::AttemptLoop {
        step_ok,
        last,
        last_reason,
        last_to,
        last_checks,
        capped_committed,
        ..
    } = state;
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

/// Whether the task drew the `fresh` arm of the continuation factor
/// (docs/CONTEXT.md): a continuation starts a new session on a handoff.
fn fresh_arm(t: &Task) -> bool {
    t.explore.get("continuation").map(String::as_str) == Some("fresh")
}

#[cfg(test)]
mod test_support;
