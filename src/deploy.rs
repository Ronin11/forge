//! `forge deploy <project> <name>`: resolve the target, check out the
//! commit to deploy, run its method, and record the result. A failed
//! check redeploys the last passing commit for the same target and asks
//! the project's most recent task for that repository what to do about
//! it (see docs/DEPLOY.md, "Rollback and the human rung").

use crate::ctx::Forge;
use crate::report::Event;
use crate::store::{DeployTarget, Task, TaskState};
use crate::{config, git, operation, unix_now};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

fn scratch_dir(f: &Forge, deploy_id: i64, suffix: &str) -> PathBuf {
    f.paths
        .worktrees
        .join(format!("deploy-{deploy_id}{suffix}"))
}

/// Check out `sha` into a scratch directory and run the target's method
/// there, cleaning the directory up either way.
#[allow(clippy::too_many_arguments)]
async fn deploy_at(
    f: &Forge,
    action: &crate::workflows::ActionDef,
    target: &DeployTarget,
    repo: &Path,
    sha: &str,
    timeout: Duration,
    scratch: &Path,
) -> Result<crate::checks::CheckResult> {
    let _ = std::fs::remove_dir_all(scratch);
    git::archive_all(repo, sha, scratch).await?;
    let r = operation::run_deploy_method(f, action, target, scratch, timeout).await;
    let _ = std::fs::remove_dir_all(scratch);
    r
}

/// Mark the project's most recent terminal task for `repo` as blocked
/// with `reason`, or file a new no-work task in that state when there is
/// none: the human rung docs/DEPLOY.md ends every failed deploy at.
fn ask(f: &Forge, project: &str, repo: &str, reason: String) -> Result<()> {
    let existing = f
        .store
        .project_tasks(project)?
        .into_iter()
        .filter(|t| {
            t.repo == repo
                && matches!(
                    t.state,
                    TaskState::Succeeded
                        | TaskState::Failed
                        | TaskState::Unverified
                        | TaskState::Withdrawn
                        | TaskState::Blocked
                )
        })
        .max_by_key(|t| t.id);
    let t = match existing {
        Some(mut t) => {
            t.state = TaskState::Blocked;
            t.reason = reason;
            t
        }
        None => {
            let mut t = Task {
                repo: repo.to_string(),
                task: "deploy question".to_string(),
                base_branch: String::new(),
                state: TaskState::Blocked,
                reason,
                created_at: unix_now(),
                workflow: "direct".to_string(),
                project: Some(project.to_string()),
                land: false,
                ..Default::default()
            };
            t.id = f.store.insert_task(&t)?;
            t
        }
    };
    f.store.update_task(&t)?;
    Ok(())
}

/// Run a deploy target now: resolve it, check out `sha` (default: the
/// repository's latest landed commit on its base branch), run the method,
/// and record what happened. On a failed check, redeploy the last passing
/// commit for the same target and ask a human about it (see
/// docs/DEPLOY.md, "When a deploy runs" and "Rollback and the human rung").
pub async fn run(f: &Forge, project: &str, name: &str, sha: Option<String>) -> Result<()> {
    let target = f
        .store
        .deploy_target(project, name)?
        .with_context(|| format!("no deploy target {name} in project {project}"))?;
    let action = operation::resolve_deploy_method(f, &target.method)?;
    let repo = PathBuf::from(&target.repo);
    let cfg = config::load_working(&repo).await?;
    let sha = match sha {
        Some(s) => git::rev_parse(&repo, &s)
            .await
            .with_context(|| format!("--sha {s}"))?,
        None => git::rev_parse(&repo, &format!("refs/heads/{}", cfg.base_branch))
            .await
            .with_context(|| format!("resolving {} on {}", cfg.base_branch, repo.display()))?,
    };
    let timeout = Duration::from_secs(cfg.check_timeout_secs);

    f.report.emit(
        0,
        Event::DeployStarted {
            project,
            target: name,
            sha: &sha,
        },
    );
    let deploy_id = f.store.start_deploy(project, name, &sha, unix_now())?;

    let r = deploy_at(
        f,
        &action,
        &target,
        &repo,
        &sha,
        timeout,
        &scratch_dir(f, deploy_id, ""),
    )
    .await?;

    if r.ok {
        f.store
            .finish_deploy(deploy_id, unix_now(), true, &r.tail, None, "")?;
        f.report.emit(
            0,
            Event::DeployFinished {
                project,
                target: name,
                sha: &sha,
                ok: true,
                rolled_back_to: None,
            },
        );
        return Ok(());
    }

    // The check failed: redeploy the last commit that passed its check on
    // this target (a target has one method for its whole life, so "the
    // same target" already means "the same method").
    let previous = f
        .store
        .deploys(project, Some(name))?
        .into_iter()
        .find(|d| d.id != deploy_id && d.check_ok == Some(true));

    let Some(previous) = previous else {
        let reason = format!(
            "the deploy of {} failed its check; there is no previous deploy to roll back to",
            short(&sha)
        );
        f.store
            .finish_deploy(deploy_id, unix_now(), false, &r.tail, None, &reason)?;
        f.report.emit(
            0,
            Event::DeployFinished {
                project,
                target: name,
                sha: &sha,
                ok: false,
                rolled_back_to: None,
            },
        );
        ask(
            f,
            project,
            &target.repo,
            format!("{reason}; here is the check's output:\n{}", r.tail),
        )?;
        return Ok(());
    };

    let rb = deploy_at(
        f,
        &action,
        &target,
        &repo,
        &previous.sha,
        timeout,
        &scratch_dir(f, deploy_id, "-rollback"),
    )
    .await?;

    let reason = format!(
        "the deploy of {} failed its check and was rolled back to {}",
        short(&sha),
        short(&previous.sha)
    );
    f.store.finish_deploy(
        deploy_id,
        unix_now(),
        false,
        &r.tail,
        Some(&previous.sha),
        &reason,
    )?;
    f.report.emit(
        0,
        Event::DeployFinished {
            project,
            target: name,
            sha: &sha,
            ok: false,
            rolled_back_to: Some(&previous.sha),
        },
    );

    let question = if rb.ok {
        format!("{reason}; here is the check's output:\n{}", r.tail)
    } else {
        format!(
            "{reason}, but the rollback's own check failed too; nothing further was attempted. Here is what each check said:\n-- {} --\n{}\n-- rollback to {} --\n{}",
            short(&sha),
            r.tail,
            short(&previous.sha),
            rb.tail
        )
    };
    ask(f, project, &target.repo, question)?;
    Ok(())
}
