//! Work that would duplicate work already live in its lineage: `forge
//! retry` refuses a task whose retry or review-demotion follow-up has not
//! ended, and `forge withdraw --abort` stops a running task that is a
//! duplicate of a live sibling.

use super::*;

/// Refuses `forge retry` of `id` when a retry or a review demotion's
/// follow-up of it is still queued, running or unverified, naming each:
/// a second copy of the same work would only race the first to the base.
/// `again` (the operator's `--again`) retries anyway.
pub fn refuse_live_descendant(f: &Forge, id: i64, again: bool) -> Result<()> {
    if again {
        return Ok(());
    }
    let live = f.store.live_descendants(id)?;
    if live.is_empty() {
        return Ok(());
    }
    bail!("{}", live_descendant_refusal(id, &live));
}

/// The refusal's text: task `id` and each live descendant with its state.
fn live_descendant_refusal(id: i64, live: &[crate::store::LineageRow]) -> String {
    let named = live
        .iter()
        .map(|r| format!("{} ({})", r.id, r.state))
        .collect::<Vec<_>>()
        .join(", ");
    format!("task {id} already has a live descendant: {named}; pass --again to retry it anyway")
}

/// Abort a running task that duplicates a live sibling (another task
/// retrying the same one, queued, running or unverified): the case
/// `withdraw` refuses. Records the decision (kind `withdraw-abort`) and
/// returns its id; the worker running the task sees it, stops the
/// attempt, and ends the task `capped` with `reason`, the way a budget cap
/// ends one (see `engine::check_abort`).
pub fn withdraw_abort(f: &Forge, id: i64, reason: &str, by: &str) -> Result<i64> {
    let Some(old) = f.store.task(id)? else {
        bail!("no task {id}");
    };
    if old.state != TaskState::Running {
        bail!(
            "task {id} is {}; --abort stops a running task (withdraw a blocked or queued one without it)",
            old.state.as_str()
        );
    }
    let siblings = f.store.live_siblings(id)?;
    if siblings.is_empty() {
        bail!("task {id} is not a duplicate of a live sibling; --abort only stops one that is");
    }
    let named = siblings
        .iter()
        .map(|r| r.id.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let decision = f.store.insert_decision_by(crate::store::InsertDecisionBy {
        task_id: id,
        repo: &old.repo,
        question: &format!("task {id} is running beside live sibling(s) {named}"),
        answer: reason,
        answered_by: by,
        citations: "",
        answered_for: old.question_to.as_deref(),
    })?;
    f.store.set_decision_kind(decision, "withdraw-abort")?;
    f.store.set_decision_retry(decision, id)?;
    f.report.emit(
        id,
        Event::Note {
            text: &format!("abort    requested: {reason} (duplicate of {named})"),
        },
    );
    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_descendant_refusal_names_every_descendant_and_the_way_past_it() {
        let row = |id, state: &str| crate::store::LineageRow {
            id,
            parent: Some(811),
            supersedes: None,
            state: state.into(),
            reason: String::new(),
            workflow: "w".into(),
            cost: 0.0,
            land: true,
            landed_sha: String::new(),
        };
        assert_eq!(
            live_descendant_refusal(811, &[row(822, "running"), row(823, "queued")]),
            "task 811 already has a live descendant: 822 (running), 823 (queued); pass --again to retry it anyway"
        );
    }
}
