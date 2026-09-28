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
/// there, cleaning the directory up either way. `deploy-self` is also told
/// whether the running worker starts successors
/// (`FORGE_WORKER_SUCCESSORS=1`), so it only stages and leaves the worker
/// unit alone.
async fn deploy_at(
    action: &operation::RunAction,
    target: &DeployTarget,
    repo: &Path,
    sha: &str,
    f: &Forge,
    timeout: Duration,
    scratch: &Path,
) -> Result<crate::checks::CheckResult> {
    git::fresh_archive(repo, sha, scratch).await?;
    let home = &f.paths.home;
    let mut extra = Vec::new();
    if target.method == SELF_METHOD {
        let successors = crate::successor::capable(home, &f.store);
        extra.push((
            "FORGE_WORKER_SUCCESSORS".to_string(),
            if successors { "1" } else { "0" }.to_string(),
        ));
    }
    let r = operation::run_deploy_method(action, target, sha, home, scratch, timeout, &extra).await;
    let _ = std::fs::remove_dir_all(scratch);
    r
}

/// The method that deploys Forge itself through the release layout.
pub(crate) const SELF_METHOD: &str = "deploy-self";

/// What a `deploy-self` target deploys, and where its tree comes from:
/// origin's base branch fetched into the kernel repository (never the
/// registered checkout's refs or working tree), and `sha` resolved there —
/// origin's tip when none is given. A commit origin's base does not
/// contain is refused, and so, without `force`, is one that is an
/// ancestor of the live release (`FORGE_HOME/bin/current`): migrations do
/// not run backwards.
async fn origin_truth(
    f: &Forge,
    repo: &Path,
    cfg: &config::Config,
    sha: Option<String>,
    force: bool,
) -> Result<(PathBuf, String)> {
    let remote = cfg.push_remote.as_deref().unwrap_or("origin");
    let url = git::remote_url(repo, remote).await.with_context(|| {
        format!(
            "{SELF_METHOD} builds from origin, and {} has no remote {remote}",
            repo.display()
        )
    })?;
    let home = &f.paths.home;
    let base = &cfg.base_branch;
    let tip = git::stage(
        home,
        repo,
        Path::new(&url),
        &format!("refs/heads/{base}"),
        &format!("refs/forge/origin/{base}"),
    )
    .await
    .with_context(|| format!("fetching {base} from {url}"))?;
    let kernel = git::kernel_repository(home, repo).await?;
    let sha = match sha {
        None => tip,
        Some(s) => {
            let full = git::rev_parse(&kernel, &format!("{s}^{{commit}}"))
                .await
                .with_context(|| format!("--sha {s}: not a commit on {url}"))?;
            if !git::is_ancestor(&kernel, &full, &tip).await {
                bail!(
                    "--sha {s}: {} is not on {url}'s {base} (at {}); {SELF_METHOD} deploys only what origin holds",
                    short(&full),
                    short(&tip)
                );
            }
            full
        }
    };
    let live = crate::release::pointed_at(&crate::release::root(home), "current");
    if let Some(live) = live
        && !force
        && let Ok(live_sha) = git::rev_parse(&kernel, &format!("{live}^{{commit}}")).await
        && live_sha != sha
        && git::is_ancestor(&kernel, &sha, &live_sha).await
    {
        bail!(
            "{} is older than the live release {} (an ancestor of it); migrations do not run \
             backwards, so it is refused. Pass --force to deploy it anyway.",
            short(&sha),
            short(&live_sha)
        );
    }
    Ok((kernel, sha))
}

/// Where a non-self target's tree comes from, and the commit to archive.
/// On landing, the sha is the commit that just landed, staged in the
/// kernel repository by `landing::try_land`'s `stage` of the task branch;
/// resolve and archive it there, since the registered checkout only gets
/// it through its own best-effort fetch, and a landing must not depend on
/// that fetch to deploy. An operator's own `forge deploy` still resolves
/// and archives against the registered checkout, the tree they mean.
async fn non_self_src(
    f: &Forge,
    repo: &Path,
    cfg: &config::Config,
    sha: Option<String>,
    task_id: Option<i64>,
) -> Result<(PathBuf, String)> {
    if task_id.is_some() {
        let sha = sha.context("an on-landing deploy always resolves a sha before deploy::run")?;
        let kernel = git::kernel_repository(&f.paths.home, repo).await?;
        let full = git::rev_parse(&kernel, &format!("{sha}^{{commit}}"))
            .await
            .with_context(|| format!("--sha {sha}"))?;
        return Ok((kernel, full));
    }
    let sha = match sha {
        Some(s) => git::rev_parse(repo, &s)
            .await
            .with_context(|| format!("--sha {s}"))?,
        None => git::rev_parse(repo, &format!("refs/heads/{}", cfg.base_branch))
            .await
            .with_context(|| format!("resolving {} on {}", cfg.base_branch, repo.display()))?,
    };
    Ok((repo.to_path_buf(), sha))
}

/// Where the rollback archives `previous_sha` from: `src`, the tree this
/// deploy came from, when it has that commit, and otherwise the kernel
/// repository, where an on-landing deploy staged it (see
/// [`non_self_src`]). An operator's `forge deploy` archives from the
/// registered checkout, which need not have a commit only a landing
/// staged, and a rollback must not fail on that.
async fn rollback_src(home: &Path, src: &Path, repo: &Path, previous_sha: &str) -> Result<PathBuf> {
    let rev = format!("{previous_sha}^{{commit}}");
    if git::rev_parse(src, &rev).await.is_ok() {
        return Ok(src.to_path_buf());
    }
    let kernel = git::kernel_repository(home, repo).await?;
    git::rev_parse(&kernel, &rev).await.with_context(|| {
        format!(
            "rolling back to {}: neither {} nor the kernel repository has it",
            short(previous_sha),
            src.display()
        )
    })?;
    Ok(kernel)
}

/// Finish `deploy_id`'s row as a failure with `e`'s text as the reason, so
/// an error after `start_deploy` never leaves the row open, and return `e`
/// unchanged for the caller to propagate. A row `run` already finished (a
/// recorded rollback, or nothing to roll back to) is left as it is; only
/// when this finishes the row, so no rollback was recorded and nobody was
/// asked, does it file the question itself.
fn record_deploy_error(
    f: &Forge,
    project: &str,
    repo: &str,
    deploy_id: i64,
    sha: &str,
    e: anyhow::Error,
) -> anyhow::Error {
    let reason = format!("{e:#}");
    let finished = f.store.finish_open_deploy(crate::store::FinishDeploy {
        id: deploy_id,
        at: unix_now(),
        check_ok: false,
        check_output: "",
        rolled_back_to: None,
        reason: &reason,
        smoke_ok: None,
        smoke_json: None,
        look_ok: None,
        look_json: None,
    });
    if matches!(finished, Ok(true)) {
        let _ = ask(
            f,
            project,
            repo,
            format!(
                "the deploy of {} failed with an error and was not rolled back; it may still be live:\n{reason}",
                short(sha)
            ),
        );
    }
    e
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

/// The last, human-shaped step (see docs/DEPLOY.md, "The deploy look"):
/// whether or not the deterministic smoke check itself passed, look at what
/// it caught. The look is told the smoke result it really had (`smoke_ok`),
/// and a look alone never rolls back a deploy whose check and smoke passed:
/// a second look must agree first. Fails `r` when the looks say so; returns
/// what goes on the deploy row.
async fn look_step(
    f: &Forge,
    target: &DeployTarget,
    deploy_id: i64,
    out_dir: &Path,
    event_task: i64,
    smoke_ok: bool,
    r: &mut crate::checks::CheckResult,
) -> Result<(Option<bool>, Option<String>)> {
    use crate::deploy_look as look;
    let note = |text: &str| f.report.emit(event_task, Event::Note { text });
    let checks_passed = r.ok;
    let v = match look::run(f, target, deploy_id, out_dir, smoke_ok, 1).await {
        Ok(Some(v)) => v,
        Ok(None) => return Ok((None, None)),
        Err(e) => {
            note(&format!("deploy-look failed: {e:#}"));
            return Ok((None, None));
        }
    };
    note(&format!(
        "deploy-look {}, {} finding(s)",
        if v.ok { "ok" } else { "not ok" },
        v.findings.len()
    ));
    let second = if look::needs_second_look(checks_passed, &v) {
        look::run(f, target, deploy_id, out_dir, smoke_ok, 2)
            .await
            .unwrap_or_else(|e| {
                note(&format!("deploy-look confirming look failed: {e:#}"));
                None
            })
    } else {
        None
    };
    if let Some(finding) = look::failing_finding(checks_passed, &v, second.as_ref()) {
        r.ok = false;
        r.tail = format!("{}\n\n-- deploy look --\n{finding}", r.tail);
    } else if look::blocking(&v).is_some() {
        note(
            "deploy-look found a blocking problem no second look confirmed; the deploy stands, a person should look",
        );
    }
    Ok((Some(v.ok), Some(serde_json::to_string(&v.findings)?)))
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
/// `force` lets a `deploy-self` target deploy a commit older than the
/// live release (see [`origin_truth`]).
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
    force: bool,
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
    // deploy-self builds origin's truth, never the registered checkout
    // (docs/OPS.md, "The running binary"): the commit and the tree to
    // archive both come from the kernel repository's copy of origin.
    let (src, sha) = if target.method == SELF_METHOD {
        origin_truth(f, &repo, &cfg, sha, force).await?
    } else {
        non_self_src(f, &repo, &cfg, sha, task_id).await?
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

    // An error anywhere below finishes the row as a failure
    // (`record_deploy_error`) rather than leaving it open: only a
    // deliberately recorded outcome (ok, a failed check, or a failed
    // rollback) returns from this block without one.
    let outcome: Result<bool> = async {
        let mut r = deploy_at(
            &action,
            &target,
            &src,
            &sha,
            f,
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
                    // A smoke step that cannot even run is a failed one: the
                    // method already succeeded, so the normal rollback and
                    // question must still follow.
                    let sr = operation::run_deploy_smoke(smoke_action, url, &out_dir, timeout)
                        .await
                        .unwrap_or_else(|e| crate::checks::CheckResult {
                            ok: false,
                            tail: format!("the smoke step could not run: {e:#}"),
                            ..Default::default()
                        });
                    let json = std::fs::read_to_string(out_dir.join("smoke.json")).ok();
                    if !sr.ok {
                        r.ok = false;
                        r.tail = format!("{}\n\n-- smoke check ({url}) --\n{}", r.tail, sr.tail);
                    }

                    let (look_ok, look_json) =
                        look_step(f, &target, deploy_id, &out_dir, event_task, sr.ok, &mut r)
                            .await?;

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

        let rb_src = rollback_src(&f.paths.home, &src, &repo, &previous.sha).await?;
        let rb = deploy_at(
            &action,
            &target,
            &rb_src,
            &previous.sha,
            f,
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
    .await;
    outcome.map_err(|e| record_deploy_error(f, project, &target.repo, deploy_id, &sha, e))
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

/// Refuse a target whose method declares `required_args` (see
/// src/builtins/operations/deploy-command.toml) that its args leave
/// missing or blank: an empty `dest` would otherwise make the method's
/// `rsync --delete` target the host's `/` (docs/REVIEW-4.md, E3-19). A
/// method the catalog does not know is left for `forge deploy` to refuse.
fn check_required_args(f: &Forge, method: &str, args: &BTreeMap<String, String>) -> Result<()> {
    let actions = crate::workflows::load_actions(&f.paths.home)?;
    let Some(def) = actions.get(method) else {
        return Ok(());
    };
    let missing: Vec<&str> = def
        .required_args
        .iter()
        .filter(|k| args.get(*k).is_none_or(|v| v.trim().is_empty()))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        bail!(
            "method {method:?} requires {}",
            missing
                .iter()
                .map(|k| format!("--arg {k}=<value>"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

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
    check_required_args(f, &spec.method, &args)?;
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
    check_required_args(f, &t.method, &t.args)?;

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

#[cfg(test)]
mod script_tests {
    use std::os::unix::fs::PermissionsExt;

    /// Run a built-in deploy method's `run` in a scratch directory with
    /// `args` as its `FORGE_ARG_*` and a fake rsync/ssh on PATH that only
    /// record being called; returns its exit code and whether either ran.
    fn run(toml_text: &str, args: &[(&str, &str)]) -> (Option<i32>, bool) {
        let v: toml::Value = toml::from_str(toml_text).unwrap();
        let argv: Vec<String> = v["run"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let log = dir.path().join("calls.log");
        for tool in ["rsync", "ssh", "systemctl"] {
            let p = bin.join(tool);
            std::fs::write(
                &p,
                format!("#!/bin/bash\necho {tool} >> {}\n", log.display()),
            )
            .unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut c = std::process::Command::new(&argv[0]);
        c.args(&argv[1..]).current_dir(dir.path()).env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        );
        for (k, v) in args {
            c.env(format!("FORGE_ARG_{}", k.to_uppercase()), v);
        }
        let status = c.status().unwrap();
        (status.code(), log.exists())
    }

    #[test]
    fn deploy_command_and_deploy_user_service_exit_1_on_an_empty_host_or_an_empty_or_root_dest() {
        let methods = [
            include_str!("builtins/operations/deploy-command.toml"),
            include_str!("builtins/operations/deploy-user-service.toml"),
        ];
        let bad: &[&[(&str, &str)]] = &[
            &[("host", ""), ("dest", "/srv/app")],
            &[("dest", "/srv/app")],
            &[("host", "box"), ("dest", "")],
            &[("host", "box")],
            &[("host", "box"), ("dest", "/")],
            &[("host", "box"), ("dest", "///")],
            &[("host", "local"), ("dest", "/")],
        ];
        for text in methods {
            for args in bad {
                let mut args = args.to_vec();
                args.push(("unit", "demo.service"));
                assert_eq!(run(text, &args), (Some(1), false), "{args:?}");
            }
        }
    }
}
