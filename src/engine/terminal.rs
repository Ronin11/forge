use super::*;

/// The end of the run on the record: the task's state and reason derived
/// from `end`, its initiative settled if this was its last task, dependents
/// released now that it landed, failed or went unverified, and the
/// `TaskDone` event.
pub(super) async fn finish(
    f: &Forge,
    t: &mut Task,
    end: &End,
    compare: Option<String>,
    wt: &Path,
) -> Result<TaskState, Fault> {
    let id = t.id;
    let attempts = f.store.attempts(id).env()?;
    t.state = end.task_state();
    t.reason = end.reason(t, attempts.len());
    t.question_to = end.question_to();
    t.finished_at = Some(unix_now());
    t.worker_pid = None;
    f.store.update_task(t).env()?;
    settle_terminal(f, t, Some(end), compare.as_deref(), wt)?;
    Ok(t.state)
}

pub(super) fn settle_terminal(
    f: &Forge,
    t: &Task,
    end: Option<&End>,
    compare: Option<&str>,
    wt: &Path,
) -> Result<(), Fault> {
    let id = t.id;
    if let Some(iid) = t.initiative {
        crate::view::maybe_settle_initiative(f, id, iid).env()?;
    }
    if t.state == TaskState::Succeeded && (!t.land || !t.landed_sha.is_empty()) {
        crate::queue::settle_superseded(f, id).env()?;
    }
    // A filing task never lands: the work its dependents waited for now
    // happens in the tasks it filed, so they follow the last of those
    // instead (the same reroute a retry carries its own dependents
    // through, see `queue::enqueue`).
    if let Some(End::Filed { last, .. }) = end {
        for d in f.store.reroute_dependents(id, *last).env()? {
            f.report.emit(
                d,
                Event::Note {
                    text: &format!("waits on task {last} now (task {id} filed its plan)"),
                },
            );
        }
    }
    // A dependent waiting on this task, blocked with a stale reason
    // because its after list has since been re-pointed here, is released
    // or given a fresh reason now that this task itself has landed,
    // failed, or gone unverified; a question or a review demotion
    // (TaskState::Blocked) settles nothing for a dependent to react to.
    if matches!(
        t.state,
        TaskState::Succeeded | TaskState::Failed | TaskState::Unverified
    ) {
        for d in f.store.release_dependents_of(id).env()? {
            f.report.emit(
                d,
                Event::Note {
                    text: "unblocked: its dependencies succeeded",
                },
            );
        }
    }

    // The worker emits blocked/failed only after demotion and retry rules.
    if !matches!(t.state, TaskState::Blocked | TaskState::Failed) {
        crate::audience::emit(f, t, compare, wt).env()?;
    }
    Ok(())
}

/// Retry settlement after a terminal write whose tail was interrupted.
pub(crate) fn settle_ready_initiatives(f: &Forge) -> anyhow::Result<()> {
    for ini in f.store.list_initiatives(None)? {
        if ini.settled_at.is_none() {
            crate::view::maybe_settle_initiative(f, 0, ini.id)?;
        }
    }
    Ok(())
}

pub(crate) fn finish_fault(f: &Forge, t: &Task) -> Result<(), Fault> {
    settle_terminal(f, t, None, None, &f.paths.worktrees.join(t.id.to_string()))
}
