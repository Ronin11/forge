//! Effects run only after the landing is durable and the repository lock is released.
use crate::ctx::Forge;
use crate::engine::{Classify, Fault};
use crate::report::Event;
use crate::store::Task;

pub(crate) struct Landing {
    pub sha: String,
    pub base_sha: String,
    pub already: bool,
}

pub(crate) fn recorded(f: &Forge, t: &Task) -> Result<Option<Landing>, Fault> {
    let Some(current) = f.store.task(t.id).env()? else {
        return Ok(None);
    };
    Ok((!current.landed_sha.is_empty()).then_some(Landing {
        sha: current.landed_sha,
        base_sha: current.base_sha,
        already: true,
    }))
}

pub(crate) fn persist(f: &Forge, t: &mut Task, landed: &Landing) -> Result<(), Fault> {
    t.landed_sha = landed.sha.clone();
    t.landed_at = Some(crate::unix_now());
    t.reason = format!("landed {} @ {}", t.base_branch, &landed.sha[..8]);
    t.pushed = true;
    f.store.update_task(t).env()
}

pub(crate) async fn run(f: &Forge, t: &mut Task, landed: &Landing) {
    if !landed.already {
        deploy_on_landing(f, t, &landed.sha).await;
        crate::assess::run_on_landing(f, t, &landed.base_sha, &landed.sha).await;
    }
}

/// After landing, run every on-landing deploy target of the task's project
/// on this repository, through the same path `forge deploy` uses
/// (`deploy::run`), tied to this task. A deploy's own failure never
/// changes the task's landed state: on a failed check, `deploy::run`
/// already emits its events and follows the rollback-and-question path.
/// Any other error (it never got that far, or a row was left open) is
/// named on the task instead of dropped.
async fn deploy_on_landing(f: &Forge, t: &Task, sha: &str) {
    let Some(project) = t.project.clone() else {
        return;
    };
    let Ok(targets) = f.store.deploy_targets(&project) else {
        return;
    };
    for target in targets
        .into_iter()
        .filter(|d| d.on_landing && d.repo == t.repo)
    {
        if let Err(e) = crate::deploy::run(
            f,
            &project,
            &target.name,
            Some(sha.to_string()),
            Some(t.id),
            false,
        )
        .await
        {
            f.report.emit(
                t.id,
                Event::Note {
                    text: &format!("deploy   {} failed: {e:#}", target.name),
                },
            );
        }
    }
}
