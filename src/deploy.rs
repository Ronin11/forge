//! `forge deploy <project> <name>`: resolve the target, check out the
//! commit to deploy, run its method, and record the result. A failed
//! check redeploys the last passing commit for the same target and asks
//! the project's most recent task for that repository what to do about
//! it (see docs/DEPLOY.md, "Rollback and the human rung").

use crate::ctx::Forge;
use crate::report::Event;
use crate::store::{DeployTarget, Task, TaskState};
use crate::{config, git, operation, unix_now};
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
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
async fn deploy_at(
    action: &operation::RunAction,
    target: &DeployTarget,
    repo: &Path,
    sha: &str,
    home: &Path,
    timeout: Duration,
    scratch: &Path,
) -> Result<crate::checks::CheckResult> {
    git::fresh_archive(repo, sha, scratch).await?;
    let r = operation::run_deploy_method(action, target, sha, home, scratch, timeout).await;
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
/// `task_id` ties the deploy row and its events to the task that landed
/// and triggered it (an on-landing target); `None` for an operator-invoked
/// `forge deploy`.
///
/// Returns whether the deploy's own check passed: `false` covers both
/// failure branches (rolled back, or nothing to roll back to), which is
/// all the exit code the CLI needs.
pub async fn run(
    f: &Forge,
    project: &str,
    name: &str,
    sha: Option<String>,
    task_id: Option<i64>,
) -> Result<bool> {
    let event_task = task_id.unwrap_or(0);
    let target = f
        .store
        .deploy_target(project, name)?
        .with_context(|| format!("no deploy target {name} in project {project}"))?;
    let action = operation::resolve_deploy_method(f, &target.method)?;
    let smoke_action = target
        .smoke_url
        .is_some()
        .then(|| operation::resolve_deploy_smoke(f))
        .transpose()?;
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
        event_task,
        Event::DeployStarted {
            project,
            target: name,
            sha: &sha,
        },
    );
    let deploy_id = f
        .store
        .start_deploy(project, name, &sha, unix_now(), task_id)?;

    let mut r = deploy_at(
        &action,
        &target,
        &repo,
        &sha,
        &f.paths.home,
        timeout,
        &scratch_dir(f, deploy_id, ""),
    )
    .await?;

    // A check that answers is not a site that works (see docs/DEPLOY.md,
    // "A deterministic smoke step"): open the target's smoke url only once
    // the check itself has passed, and let it fail the deploy too.
    let (smoke_ok, smoke_json, look_ok, look_json) = if r.ok {
        match (&target.smoke_url, &smoke_action) {
            (Some(url), Some(smoke_action)) => {
                let out_dir = f.paths.home.join("deploys").join(deploy_id.to_string());
                let sr = operation::run_deploy_smoke(smoke_action, url, &out_dir, timeout).await?;
                let json = std::fs::read_to_string(out_dir.join("smoke.json")).ok();
                if !sr.ok {
                    r.ok = false;
                    r.tail = format!("{}\n\n-- smoke check ({url}) --\n{}", r.tail, sr.tail);
                }

                // The last, human-shaped step (see docs/DEPLOY.md, "The
                // deploy look"): whether or not the deterministic smoke
                // check itself passed, look at what it caught.
                let (look_ok, look_json) =
                    match crate::deploy_look::run(f, &target, deploy_id, &out_dir).await {
                        Ok(Some(v)) => {
                            f.report.emit(
                                event_task,
                                Event::Note {
                                    text: &format!(
                                        "deploy-look {}, {} finding(s)",
                                        if v.ok { "ok" } else { "not ok" },
                                        v.findings.len()
                                    ),
                                },
                            );
                            if let Some(blocking) =
                                v.findings.iter().find(|fnd| fnd.severity == "blocking")
                            {
                                r.ok = false;
                                r.tail = format!(
                                    "{}\n\n-- deploy look --\n{}",
                                    r.tail, blocking.finding
                                );
                            }
                            (Some(v.ok), Some(serde_json::to_string(&v.findings)?))
                        }
                        Ok(None) => (None, None),
                        Err(e) => {
                            f.report.emit(
                                event_task,
                                Event::Note {
                                    text: &format!("deploy-look failed: {e:#}"),
                                },
                            );
                            (None, None)
                        }
                    };

                (Some(sr.ok), json, look_ok, look_json)
            }
            _ => (None, None, None, None),
        }
    } else {
        (None, None, None, None)
    };

    if r.ok {
        f.store.finish_deploy(crate::store::FinishDeploy {
            id: deploy_id,
            at: unix_now(),
            check_ok: true,
            check_output: &r.tail,
            rolled_back_to: None,
            reason: "",
            smoke_ok,
            smoke_json: smoke_json.as_deref(),
            look_ok,
            look_json: look_json.as_deref(),
        })?;
        f.report.emit(
            event_task,
            Event::DeployFinished {
                project,
                target: name,
                sha: &sha,
                ok: true,
                rolled_back_to: None,
            },
        );
        return Ok(true);
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
        f.store.finish_deploy(crate::store::FinishDeploy {
            id: deploy_id,
            at: unix_now(),
            check_ok: false,
            check_output: &r.tail,
            rolled_back_to: None,
            reason: &reason,
            smoke_ok,
            smoke_json: smoke_json.as_deref(),
            look_ok,
            look_json: look_json.as_deref(),
        })?;
        f.report.emit(
            event_task,
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
        return Ok(false);
    };

    let rb = deploy_at(
        &action,
        &target,
        &repo,
        &previous.sha,
        &f.paths.home,
        timeout,
        &scratch_dir(f, deploy_id, "-rollback"),
    )
    .await?;

    let reason = format!(
        "the deploy of {} failed its check and was rolled back to {}",
        short(&sha),
        short(&previous.sha)
    );
    f.store.finish_deploy(crate::store::FinishDeploy {
        id: deploy_id,
        at: unix_now(),
        check_ok: false,
        check_output: &r.tail,
        rolled_back_to: Some(&previous.sha),
        reason: &reason,
        smoke_ok,
        smoke_json: smoke_json.as_deref(),
        look_ok,
        look_json: look_json.as_deref(),
    })?;
    f.report.emit(
        event_task,
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
    Ok(false)
}

/// `forge project deploy add`'s fields, parsed by clap but not yet
/// resolved against the store or the filesystem.
pub struct TargetSpec {
    pub project: String,
    pub name: String,
    pub repo: PathBuf,
    pub scope: Option<String>,
    pub method: String,
    pub args: Vec<String>,
    pub check: Option<String>,
    pub smoke: Option<String>,
    pub on_landing: bool,
}

/// `forge project deploy set`'s fields: each `Some`/non-empty one
/// replaces its field on the stored target, everything else is left
/// alone (see [`set_target`]).
pub struct TargetChanges {
    pub repo: Option<PathBuf>,
    pub scope: Option<String>,
    pub method: Option<String>,
    pub args: Vec<String>,
    pub check: Option<String>,
    pub smoke: Option<String>,
    pub on_landing: bool,
    pub no_on_landing: bool,
}

/// Methods that supply their own check when the target declares none:
/// `deploy-static` fetches its url, `deploy-self` fetches the web
/// client's `/tasks`.
fn has_default_check(method: &str) -> bool {
    matches!(method, "deploy-static" | "deploy-self")
}

/// A deploy target's own dedicated flags, kept out of `--arg` so a typo
/// like `--arg method=...` fails loudly instead of landing an argument
/// the method never reads.
const RESERVED_ARG_KEYS: &[&str] = &[
    "project",
    "name",
    "repo",
    "scope",
    "method",
    "check",
    "smoke",
    "on_landing",
];

/// Parse repeated `<key>=<value>` pairs from `--arg` into a map, in the
/// order clap collected them (last write wins on a repeated key).
/// Refuses a pair with no `=` and a key that shadows one of the target's
/// own flags (see [`RESERVED_ARG_KEYS`]).
pub fn parse_target_args(pairs: &[String]) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for pair in pairs {
        let (k, v) = pair
            .split_once('=')
            .with_context(|| format!("--arg {pair:?}: expected <key>=<value>"))?;
        if RESERVED_ARG_KEYS.contains(&k) {
            bail!("--arg {k:?}: use --{k} instead of --arg");
        }
        map.insert(k.to_string(), v.to_string());
    }
    Ok(map)
}

/// Declare a deploy target: resolve its repository to an absolute path,
/// its comma-separated `scope` to the JSON array the store keeps, and
/// its `--arg`s to a map, then insert it (see docs/DEPLOY.md, "A
/// target"). `check` is required except for the methods that default
/// their own (see [`has_default_check`]), which store an empty one.
pub fn add_target(f: &Forge, spec: TargetSpec) -> Result<DeployTarget> {
    f.store
        .project(&spec.project)?
        .with_context(|| format!("no project {}", spec.project))?;
    let repo = spec
        .repo
        .canonicalize()
        .with_context(|| format!("--repo {}", spec.repo.display()))?;
    let scope_json = spec
        .scope
        .map(|s| serde_json::to_string(&s.split(',').collect::<Vec<_>>()))
        .transpose()?;
    let args = parse_target_args(&spec.args)?;
    let check = match spec.check {
        Some(c) => c,
        None if has_default_check(&spec.method) => String::new(),
        None => bail!("--check is required for method {:?}", spec.method),
    };
    let target = DeployTarget {
        project: spec.project,
        name: spec.name,
        repo: repo.display().to_string(),
        scope: scope_json,
        method: spec.method,
        args,
        check_cmd: check,
        on_landing: spec.on_landing,
        smoke_url: spec.smoke,
    };
    f.store.add_deploy_target(&target)?;
    Ok(target)
}

/// Change a deploy target's fields, replacing only the ones given: the
/// same shape as [`add_target`], but starting from the stored target and
/// merging each change onto it (`args` onto the args map, everything
/// else replacing its field whole).
pub fn set_target(
    f: &Forge,
    project: &str,
    name: &str,
    changes: TargetChanges,
) -> Result<DeployTarget> {
    let mut t = f
        .store
        .deploy_target(project, name)?
        .with_context(|| format!("no deploy target {name} in project {project}"))?;

    if let Some(repo) = changes.repo {
        let repo = repo
            .canonicalize()
            .with_context(|| format!("--repo {}", repo.display()))?;
        t.repo = repo.display().to_string();
    }
    if let Some(scope) = changes.scope {
        t.scope = Some(serde_json::to_string(
            &scope.split(',').collect::<Vec<_>>(),
        )?);
    }
    if let Some(method) = changes.method {
        t.method = method;
    }
    for (k, v) in parse_target_args(&changes.args)? {
        t.args.insert(k, v);
    }
    if let Some(check) = changes.check {
        t.check_cmd = check;
    }
    if let Some(smoke) = changes.smoke {
        t.smoke_url = Some(smoke);
    }
    if changes.on_landing {
        t.on_landing = true;
    } else if changes.no_on_landing {
        t.on_landing = false;
    }
    if t.check_cmd.is_empty() && !has_default_check(&t.method) {
        bail!("--check is required for method {:?}", t.method);
    }

    f.store.update_deploy_target(&t)?;
    Ok(t)
}

#[cfg(test)]
mod arg_tests {
    use super::parse_target_args;

    fn pairs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_target_args_collects_a_valid_set_last_write_wins() {
        let map = parse_target_args(&pairs(&["host=box1", "dest=/srv/app", "host=box2"])).unwrap();
        assert_eq!(map.get("host").map(String::as_str), Some("box2"));
        assert_eq!(map.get("dest").map(String::as_str), Some("/srv/app"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn parse_target_args_refuses_a_pair_with_no_equals_sign() {
        let err = parse_target_args(&pairs(&["hostbox1"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("hostbox1"), "{err}");
        assert!(err.contains("expected <key>=<value>"), "{err}");
    }

    #[test]
    fn parse_target_args_refuses_a_key_reserved_for_a_dedicated_flag() {
        let err = parse_target_args(&pairs(&["method=deploy-command"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("method"), "{err}");
        assert!(err.contains("--method"), "{err}");
    }
}
