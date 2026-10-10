use super::*;

/// Whether a code step already verified in the record: the run stopped
/// before the steps that would vouch for it, but the checks passed on
/// what it left.
pub(crate) fn code_verified(resolved: &workflows::Resolved, done: &HashSet<i64>) -> bool {
    resolved
        .steps
        .iter()
        .enumerate()
        .any(|(i, s)| s.action.contract == Contract::Code && done.contains(&(i as i64 + 1)))
}

/// Whether a capped task's own record shows a verified code step, so a
/// human's `forge land` may take the branch.
pub(crate) fn landable_capped(t: &Task, attempts: &[crate::store::Attempt]) -> bool {
    t.state == TaskState::Capped
        && serde_json::from_str::<workflows::Resolved>(&t.actions_json)
            .is_ok_and(|r| code_verified(&r, &resume_done(attempts)))
}

/// The budget check made at claim and before every attempt: what is
/// spent plus what the next attempt is expected to cost (the task's mean
/// attempt so far, else the workflow's measured mean) against the cap.
/// When it would cross, the task ends `capped`: the session and the last
/// handoff go on the row, and the reason names spent and cap.
pub(super) async fn check_cap(
    f: &Forge,
    t: &mut Task,
    resolved: &workflows::Resolved,
    done: &HashSet<i64>,
    cap: f64,
    wt: &Path,
) -> Result<Option<End>, Fault> {
    let spent = f.store.task_cost(t.id).env()?;
    let expected = f.store.expected_attempt_cost(t.id, &t.workflow).env()?;
    if spent < cap && spent + expected <= cap {
        return Ok(None);
    }
    let attempts = f.store.attempts(t.id).env()?;
    let last = attempts.iter().rev().find(|a| a.is_agent());
    if let Some(a) = last {
        t.session_id = a.session_id.clone();
        t.handoff = crate::handoff::build(f, t, wt, &a.start_sha, Path::new(&a.log_path)).await;
    }
    let mut reason = format!("${spent:.2} of ${cap:.2}");
    if code_verified(resolved, done) {
        let next = resolved
            .steps
            .iter()
            .enumerate()
            .find(|(i, s)| s.action.kind == Kind::Directive && !done.contains(&(*i as i64 + 1)))
            .map_or("the remaining steps", |(_, s)| s.action.name.as_str());
        reason.push_str(&format!("; code step verified, {next} not run"));
    } else {
        reason.push_str(&format!(
            "; the next attempt is expected to cost ${expected:.2}"
        ));
    }
    Ok(Some(End::Capped {
        reason,
        pushes: last.is_some(),
    }))
}

/// An operator's `forge withdraw --abort` on this running task (a
/// duplicate of a live sibling): it ends `capped`, as a cost cap would end
/// it, with the decision's reason. Nothing is pushed: the sibling carries
/// the work.
pub(super) fn check_abort(f: &Forge, t: &Task) -> Result<Option<End>, Fault> {
    Ok(f.store.abort_requested(t.id).env()?.map(|why| End::Capped {
        reason: format!("aborted: {why}"),
        pushes: false,
    }))
}

/// What a task that stopped on its budget cap needs to have its commits
/// judged: the run's config and remote, and its op and attempt counters.
pub(super) struct Salvage<'a> {
    pub(super) f: &'a Forge,
    pub(super) t: &'a mut Task,
    pub(super) cfg: &'a config::Config,
    pub(super) run: &'a mut Run,
    pub(super) repo: &'a Path,
    pub(super) wt: &'a Path,
    pub(super) remote_url: &'a Option<String>,
    pub(super) base_cfg: &'a config::Config,
    pub(super) attempt_no: &'a mut i64,
}

/// A task that stopped on its budget cap with commits past its base: the
/// repository's checks run on the branch as it stands, no agent, the way
/// `forge adopt` judges a hand-made branch. Passing commits land (or are
/// held for a human per `--no-land`); failing ones block the task with
/// the failing checks' lines, so a human or a new task picks them up.
/// Any other end, an abort (a sibling carries that work), and a cap that
/// left nothing to judge come back as they were.
pub(super) async fn salvage(s: Salvage<'_>, end: End) -> Result<End, Fault> {
    let End::Capped {
        reason: capped,
        pushes: true,
    } = &end
    else {
        return Ok(end);
    };
    let capped = capped.clone();
    Ok(judge(s, &capped).await?.unwrap_or(end))
}

/// `salvage` for a cap with `capped` as its reason; `None` when there was
/// nothing to judge.
async fn judge(s: Salvage<'_>, capped: &str) -> Result<Option<End>, Fault> {
    let (t, wt) = (&s.t, s.wt);
    if t.base_sha.is_empty() || !wt.join(".git").exists() {
        return Ok(None);
    }
    let commits = git::count_commits(wt, &t.base_sha).await.task()?;
    if commits == 0 {
        return Ok(None);
    }
    let left = format!("{commits} commit(s) past {}", t.base_branch);
    let adopt = format!("forge adopt {} {}", s.repo.display(), t.branch);
    if !git::dirty_paths(wt).await.task()?.is_empty() {
        return Ok(Some(End::Capped {
            reason: format!(
                "{capped}; left {left} and a dirty tree, so no check ran on them; run {adopt} to verify and land the commits as they are"
            ),
            pushes: true,
        }));
    }
    let v = verify_as_it_stands(&s, &left).await?;
    s.run.seq += 1;
    if v.state != AttemptState::Succeeded {
        return Ok(Some(End::Blocked {
            reason: failing_reason(capped, &left, &v, &adopt, t.id),
            demoted: true,
            to: None,
        }));
    }
    let passed = format!("{capped}; the {left} pass the checks as they stand");
    land_passed(s, &passed, &adopt).await.map(Some)
}

/// The repository's checks on the worktree's commits, unchanged, recorded
/// as a kernel `verify` row at the run's next sequence number.
async fn verify_as_it_stands(s: &Salvage<'_>, left: &str) -> Result<verify::Verdict, Fault> {
    let (f, t, repo, wt) = (s.f, &s.t, s.repo, s.wt);
    let id = t.id;
    f.report.emit(
        id,
        Event::Note {
            text: &format!("capped   left {left}; running the checks on the branch as it stands"),
        },
    );
    let head = git::rev_parse(wt, "HEAD").await.task()?;
    let timer = Timer::now();
    let overlay = overlay_refs(repo, id, Some(&t.verify_base)).await;
    let v = verify::verify_integration(&Subject {
        task_id: id,
        repo,
        worktree: wt,
        base_sha: &t.base_sha,
        start_sha: &t.base_sha,
        branch: &t.branch,
        cfg: s.cfg,
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
    let ok = v.state == AttemptState::Succeeded;
    op(
        f,
        id,
        &timer,
        OpRow {
            seq: s.run.seq + 1,
            name: "verify",
            kernel: true,
            ok,
            exit: None,
            detail: &if ok {
                format!("{left} verified as they stand @ {}", &head[..8])
            } else {
                v.reason.clone()
            },
            attempt_id: None,
            // The commit this verify judged, for the landing guard
            // (`landing::last_verified_sha`).
            output: if ok { &head } else { "" },
        },
    )?;
    Ok(v)
}

/// Commits that passed as they stand: landed through the integrator, or
/// held for a human when the task, its trust level or the repository does
/// not land. A landing that fails leaves the task capped, saying so.
async fn land_passed(s: Salvage<'_>, passed: &str, adopt: &str) -> Result<End, Fault> {
    let Salvage {
        f,
        t,
        run,
        repo,
        remote_url,
        base_cfg,
        attempt_no,
        ..
    } = s;
    let id = t.id;
    f.report.emit(
        id,
        Event::Note {
            text: "capped   the commits it left pass the checks as they stand",
        },
    );
    if !t.land {
        return Ok(End::Held(format!(
            "{passed}; left for a human (--no-land): forge land {id} lands it"
        )));
    }
    if !f.trust_policy(t.trust).auto_land {
        return Ok(End::Unverified(format!(
            "{passed}; trust {}: tasks at this level do not land themselves; land it with forge land {id} or from the inbox page",
            t.trust.as_str()
        )));
    }
    let (Some(url), Some(remote)) = (remote_url, &base_cfg.push_remote) else {
        return Ok(End::Held(format!(
            "{passed}; the repository has no push remote to land on"
        )));
    };
    let lock = crate::landing::repo_lock(f, repo).await?;
    let mut seq = run.seq;
    let outcome = integrate(f, t, url, remote, &mut seq, attempt_no, &lock).await?;
    run.seq = seq;
    match outcome {
        Integrate::Landed(landed) => {
            crate::landing::effects::persist(f, t, &landed)?;
            drop(lock);
            crate::landing::effects::run(f, t, &landed).await;
            Ok(End::Landed(landed.sha))
        }
        Integrate::Rewind { first, .. } | Integrate::Failed(first) => Ok(End::Capped {
            reason: format!(
                "{passed}, but landing them failed: {first}; merge {} into {} and run {adopt}",
                t.base_branch, t.branch
            ),
            pushes: true,
        }),
    }
}

/// A blocked capped task's reason: the cap, which checks failed on the
/// commits it left, their last lines, and what picks the branch up.
fn failing_reason(capped: &str, left: &str, v: &verify::Verdict, adopt: &str, id: i64) -> String {
    let failed: Vec<&CheckResult> = v.checks.iter().filter(|c| !c.ok).collect();
    let names: Vec<&str> = failed.iter().map(|c| c.name.as_str()).collect();
    let mut reason = if names.is_empty() {
        format!(
            "{capped}; the {left} fail the checks as they stand: {}",
            v.reason
        )
    } else {
        format!(
            "{capped}; the {left} fail check {} as they stand",
            names.join(", ")
        )
    };
    for c in &failed {
        let tail = crate::checks::last_lines(&c.tail, 10);
        reason.push_str(&format!("\n- {} {}:", c.level, c.name));
        if !tail.trim().is_empty() {
            reason.push('\n');
            reason.push_str(&tail);
        }
    }
    reason.push_str(&format!(
        "\nfix the branch and run {adopt}, or forge retry {id} to give it to an agent again"
    ));
    reason
}
