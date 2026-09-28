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
