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

/// The decision kind of a deploy receipt on the landed task.
pub const RECEIPT_KIND: &str = "deploy-receipt";

/// After landing, run every on-landing deploy target of the task's project
/// on this repository, through the same path `forge deploy` uses
/// (`deploy::run`), tied to this task, and write the **receipt** on the
/// task: a `deploy-receipt` decision per target (ok, failed, or error,
/// citing the deploy row and the question a failure filed) and the same
/// in words on the task's reason, so "landed" is never the whole story
/// when a deploy followed (docs/DEPLOY.md, "When a deploy runs"). A
/// deploy's own failure never changes the task's landed state: on a
/// failed check, `deploy::run` already emits its events and follows the
/// rollback-and-question path. Any other error (it never got that far, or
/// a row was left open) is named on the task instead of dropped.
async fn deploy_on_landing(f: &Forge, t: &mut Task, sha: &str) {
    let Some(project) = t.project.clone() else {
        return;
    };
    let Ok(targets) = f.store.deploy_targets(&project) else {
        return;
    };
    let repo = t.repo.clone();
    let targets: Vec<_> = targets
        .into_iter()
        .filter(|d| d.on_landing && d.repo == repo)
        .collect();
    for target in targets {
        let outcome = crate::deploy::run(
            f,
            &project,
            &target.name,
            Some(sha.to_string()),
            Some(t.id),
            false,
        )
        .await;
        if let Err(e) = &outcome {
            f.report.emit(
                t.id,
                Event::Note {
                    text: &format!("deploy   {} failed: {e:#}", target.name),
                },
            );
        }
        if let Err(e) = receipt(f, t, &target.name, sha, &outcome) {
            f.report.emit(
                t.id,
                Event::Note {
                    text: &format!(
                        "deploy   {}: the receipt was not recorded: {e:#}",
                        target.name
                    ),
                },
            );
        }
    }
}

/// Write one target's receipt: the decision row, and the note on the
/// task's reason.
fn receipt(
    f: &Forge,
    t: &mut Task,
    target: &str,
    sha: &str,
    outcome: &anyhow::Result<bool>,
) -> anyhow::Result<()> {
    let short = &sha[..sha.len().min(8)];
    let row = f
        .store
        .deploys_for_task(t.id)?
        .into_iter()
        .rfind(|d| d.target == target && d.sha == sha);
    let question = match &row {
        Some(d) => f.store.task_for_deploy(d.id)?,
        None => None,
    };
    let (answer, note) = match (outcome, &row) {
        (Ok(true), _) => (
            format!("ok: {target} @ {short}"),
            format!("deployed {target}"),
        ),
        (Ok(false), Some(d)) => (
            format!("failed: {target} @ {short}: {}", d.reason),
            format!("deploy {target} failed: {}", d.reason),
        ),
        (Ok(false), None) => (
            format!("failed: {target} @ {short} (no deploy row)"),
            format!("deploy {target} failed"),
        ),
        (Err(e), _) => (
            format!("error: {target} @ {short}: {e:#}"),
            format!("deploy {target} failed: {e:#}"),
        ),
    };
    let mut citations: Vec<String> = row.iter().map(|d| format!("deploy {}", d.id)).collect();
    if let Some(q) = &question {
        citations.push(format!("task {}", q.id));
    }
    let id = f.store.insert_decision_by(crate::store::InsertDecisionBy {
        task_id: t.id,
        repo: &t.repo,
        question: &format!("did the on-landing deploy of {target} at {short} go through?"),
        answer: &answer,
        answered_by: "deploy",
        citations: &citations.join(", "),
        answered_for: None,
    })?;
    f.store.set_decision_kind(id, RECEIPT_KIND)?;
    if let Some(q) = &question {
        f.store.set_decision_retry(id, q.id)?;
    }
    let note = match &question {
        Some(q) => format!("{note} (question task {})", q.id),
        None => note,
    };
    t.reason = format!("{}; {note}", t.reason);
    f.store.update_task(t)?;
    Ok(())
}
