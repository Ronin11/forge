//! Landing after the last step, and publishing the branch once the run
//! ends: the two ways a verified branch leaves the worker.

use super::*;

/// Candidate, base configuration, and retry state for a landing attempt.
pub(super) struct TryLand<'a> {
    pub(super) f: &'a Forge,
    pub(super) t: &'a mut Task,
    pub(super) cfg: &'a mut config::Config,
    pub(super) resolved: &'a workflows::Resolved,
    pub(super) run: &'a mut Run,
    pub(super) repo: &'a Path,
    pub(super) wt: &'a Path,
    pub(super) remote_url: &'a Option<String>,
    pub(super) base_cfg: &'a config::Config,
    pub(super) attempt_no: &'a mut i64,
    pub(super) task_cap: f64,
}

/// Landing, after the last step and before anything is pushed: the
/// branch verified against the base it started from, and lands only if it
/// also verifies with the base as it is now. `Some(end)` ends the run;
/// `None` means the landing failed and the run was rewound to the code
/// step with the integrator's feedback, the base moved under it and the
/// config reloaded, and the caller goes round again.
pub(super) async fn try_land(args: TryLand<'_>) -> Result<Option<End>, Fault> {
    let TryLand {
        f,
        t,
        cfg,
        resolved,
        run,
        repo,
        wt,
        remote_url,
        base_cfg,
        attempt_no,
        task_cap,
    } = args;
    let id = t.id;
    // Landing: a kernel operation, after the last step and before anything
    // is pushed. The branch verified against the base it started from; it
    // lands only if it also verifies with the base as it is now.
    if !t.land {
        return Ok(Some(End::Verified));
    }
    // A level that may not land itself ends here, the checks passed: a
    // person lands it (`forge land`, the inbox page), and the on-landing
    // assessment runs then.
    if !f.trust_policy(t.trust).auto_land {
        let reason = format!(
            "trust {}: the checks passed, but tasks at this level do not land themselves; land it with forge land {id} or from the inbox page",
            t.trust.as_str()
        );
        f.report.emit(
            id,
            Event::Note {
                text: &format!("land     skipped: {reason}"),
            },
        );
        return Ok(Some(End::Unverified(reason)));
    }
    let (Some(url), Some(remote)) = (&remote_url, &base_cfg.push_remote) else {
        f.report.emit(
            id,
            Event::Note {
                text: "land     skipped: the repository has no push remote",
            },
        );
        return Ok(Some(End::Verified));
    };
    // A change to the repository's risky paths lands on a verdict, not on
    // its checks alone (`risky`): Jev on the review's prose, else the
    // operator, with the branch verified and pushed.
    if let Some(hit) = crate::risky::classify(wt, &t.base_sha, cfg).await.task()? {
        let review = last_review(f, t, resolved)?;
        let verdict = crate::risky::judge(f, t, &hit, review.as_deref()).await;
        crate::risky::record(f, t, &hit, &verdict).env()?;
        match verdict {
            crate::risky::Verdict::Approve(p) => f.report.emit(
                id,
                Event::Note {
                    text: &format!(
                        "risky    {}: the review approves (p {p:.2})",
                        hit.describe()
                    ),
                },
            ),
            crate::risky::Verdict::Hold(why) => {
                f.report.emit(
                    id,
                    Event::Note {
                        text: &format!("land     held for a verdict: {why}"),
                    },
                );
                return Ok(Some(End::Unverified(crate::risky::hold_reason(
                    id, &hit, &why,
                ))));
            }
        }
    }
    let mut seq = run.seq;
    let lock = crate::landing::repo_lock(f, repo).await?;
    let outcome = integrate(f, t, url, remote, &mut seq, attempt_no, &lock).await?;
    run.seq = seq;
    match outcome {
        Integrate::Landed(landed) => {
            crate::landing::effects::persist(f, t, &landed)?;
            drop(lock);
            crate::landing::effects::run(f, t, &landed).await;
            Ok(Some(End::Landed(landed.sha)))
        }
        Integrate::Rewind {
            feedback,
            first,
            base_sha,
        } => {
            t.base_sha = base_sha;
            f.store.update_task(t).env()?;
            let Some(c_idx) = (0..resolved.steps.len())
                .rev()
                .find(|&i| resolved.steps[i].action.contract == Contract::Code)
            else {
                return Ok(Some(End::Failed {
                    reason: format!("landing failed: {first}"),
                    counted: true,
                    pushes: false,
                }));
            };
            let c_seq = c_idx as i64 + 1;
            let c_used = run.used_at(c_seq);
            let spent = f.store.task_cost(id).env()?;
            if c_used < t.max_attempts && spent < task_cap {
                f.report.emit(
                    id,
                    Event::Note {
                        text: &format!(
                            "land     back to {} for another attempt",
                            resolved.steps[c_idx].action.name
                        ),
                    },
                );
                // The base moved: its checks and rules are the ones that apply now.
                *cfg = config::load_at(repo, wt, &t.base_sha).await.task()?;
                cfg.protected = f.effective_protected(t, &cfg.protected);
                f.allow_egress(wt, cfg, t.trust, Some(&t.provider));
                run.rewind(c_idx, feedback);
                return Ok(None);
            }
            // The task stays failed (`forge land` takes only a verified
            // one), so the message names the command that does work.
            let recover = format!("forge adopt {} {}", t.repo, t.branch);
            Ok(Some(End::Failed {
                reason: if spent >= task_cap {
                    format!(
                        "landing failed: {first}; the task budget is spent (${spent:.2} of ${task_cap:.2}), so the verified branch is pushed; to land it: {recover}"
                    )
                } else {
                    format!(
                        "landing failed after {c_used} attempt(s): {first}; the verified branch is pushed; to land it: {recover}"
                    )
                },
                counted: true,
                pushes: true,
            }))
        }
        Integrate::Failed(reason) => Ok(Some(End::Failed {
            reason: format!("landing failed: {reason}"),
            counted: true,
            pushes: false,
        })),
    }
}

/// Publish the branch: pushed to the remote when there is one (a failed
/// push is the `End` returned, since verified work that could not be
/// published is not a success, whatever the last attempt did), else kept
/// in the registered repository where a human can merge it. Returns the
/// compare url when the remote has one.
pub(super) async fn publish(
    f: &Forge,
    t: &mut Task,
    wt: &Path,
    repo: &Path,
    remote_url: &Option<String>,
    seq: i64,
) -> Result<(Option<String>, Option<End>), Fault> {
    let id = t.id;
    let mut compare: Option<String> = None;
    let mut failed: Option<End> = None;
    if let Some(url) = &remote_url {
        let timer = Timer::now();
        match git::push(&f.paths.home, repo, wt, url, &t.branch).await {
            Ok(_) => {
                t.pushed = true;
                compare = git::compare_url(url, &t.base_branch, &t.branch);
                f.report.emit(
                    id,
                    Event::Pushed {
                        remote: url,
                        branch: &t.branch,
                    },
                );
                op(
                    f,
                    id,
                    &timer,
                    OpRow {
                        seq,
                        name: "push",
                        kernel: true,
                        ok: true,
                        exit: None,
                        detail: &t.branch,
                        attempt_id: None,
                        output: "",
                    },
                )?;
            }
            Err(e) => {
                // Verified work that could not be published is not a
                // success, whatever the last attempt did.
                failed = Some(End::Failed {
                    reason: format!("push failed: {e:#}"),
                    counted: false,
                    pushes: false,
                });
                f.report.emit(
                    id,
                    Event::PushFailed {
                        error: &format!("{e:#}"),
                    },
                );
                op(
                    f,
                    id,
                    &timer,
                    OpRow {
                        seq,
                        name: "push",
                        kernel: true,
                        ok: false,
                        exit: None,
                        detail: &format!("{e:#}"),
                        attempt_id: None,
                        output: "",
                    },
                )?;
            }
        }
    } else {
        // No remote: the branch still leaves the worktree, into the
        // registered repository, where `git branch` shows it and a
        // human can merge it.
        match git::push_to_repo(&f.paths.home, repo, wt, &t.branch).await {
            Ok(_) => f.report.emit(
                id,
                Event::Note {
                    text: &format!(
                        "kept     {} in {} (no remote to push to)",
                        t.branch,
                        repo.display()
                    ),
                },
            ),
            Err(e) => f.report.emit(
                id,
                Event::Note {
                    text: &format!(
                        "kept     could not put {} in the repository: {e:#}",
                        t.branch
                    ),
                },
            ),
        }
        f.report.emit(id, Event::PushSkipped);
    }
    Ok((compare, failed))
}

/// The prose of the run's last successful review step, for the risky
/// verdict: the envelope's summary and claims, else the result text.
/// `None` when the workflow has no review contract or none succeeded.
fn last_review(
    f: &Forge,
    t: &Task,
    resolved: &workflows::Resolved,
) -> Result<Option<String>, Fault> {
    let names: Vec<&str> = resolved
        .steps
        .iter()
        .filter(|s| s.action.contract == Contract::Review)
        .map(|s| s.action.name.as_str())
        .collect();
    if names.is_empty() {
        return Ok(None);
    }
    let attempts = f.store.attempts(t.id).env()?;
    Ok(attempts
        .iter()
        .rev()
        .filter(|a| names.contains(&a.step.as_str()) && a.state == AttemptState::Succeeded)
        .find_map(|a| {
            let text = match serde_json::from_str::<crate::envelope::Envelope>(&a.envelope_json) {
                Ok(e) => {
                    let mut s = e.summary.clone();
                    for c in &e.claims {
                        s.push_str(&format!("\n- {}: {}", c.claim, c.evidence));
                    }
                    s
                }
                Err(_) => a.result_text.clone(),
            };
            (!text.trim().is_empty()).then_some(text)
        }))
}
