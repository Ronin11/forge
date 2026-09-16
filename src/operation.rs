//! Operations: a workflow step that is a command, not an agent. Run in
//! the task's clone (or a scratch copy of base for one that reads the
//! hidden tests), with the task's facts as environment; what it prints
//! becomes a product (context, interface) or a change the kernel commits
//! and verifies.

use crate::ctx::Forge;
use crate::engine::{Classify, Fault, OpRow, Timer, op};
use crate::landing::overlay_refs;
use crate::report::Event;
use crate::store::{AttemptState, DeployTarget, Task};
use crate::verify::{self, Subject};
use crate::workflows::{self, ResolvedStep};
use crate::{checks, config, git};
use anyhow::Context as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What an operation is told about its task, as environment. Facts only,
/// each one already recorded on the task.
fn operation_env(
    t: &Task,
    cfg: &config::Config,
    step: &ResolvedStep,
    prev_sha: &str,
    hot_files: &[String],
    cache_dir: &Path,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = [
        ("FORGE_TASK_ID", t.id.to_string()),
        ("FORGE_WORKFLOW", t.workflow.clone()),
        ("FORGE_STEP", step.action.name.clone()),
        ("FORGE_BASE_BRANCH", t.base_branch.clone()),
        ("FORGE_BASE_SHA", t.base_sha.clone()),
        ("FORGE_BRANCH", t.branch.clone()),
        ("FORGE_NAMESPACE", cfg.namespace.join(" ")),
        ("FORGE_PREV_SHA", prev_sha.to_string()),
        ("FORGE_TASK", t.task.clone()),
        (
            "FORGE_BIN_DIR",
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.display().to_string()))
                .unwrap_or_default(),
        ),
        ("FORGE_HOT_FILES", hot_files.join(",")),
        ("FORGE_CACHE_DIR", cache_dir.display().to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    if step.action.reads_verify_ref() {
        env.push(("FORGE_VERIFY_REF".into(), format!("verify/{}", t.id)));
    }
    env
}

fn op_scratch_dir(worktree: &str) -> PathBuf {
    PathBuf::from(format!("{worktree}-op"))
}

/// A user operation: one command in the sandbox, or the repository's
/// declared check of that name. Exit code decides. A `check` the
/// repository does not declare is skipped and recorded as such.
///
/// Where it runs: the clone, unless it consumes `verify_ref`, in which
/// case a scratch copy of base with the task's hidden tests overlaid, so
/// the coder's tree never holds them. What it may produce: `interface`,
/// its stdout, handed to the next code directive; `branch`, in which case
/// the kernel commits what it changed and verifies the result exactly as
/// after a directive, minus the envelope rows, because there is no claim.
pub(crate) async fn run_operation(
    f: &Forge,
    t: &mut Task,
    cfg: &config::Config,
    step: &ResolvedStep,
    seq: i64,
) -> Result<(bool, String), Fault> {
    let wt = PathBuf::from(&t.worktree);
    let repo = PathBuf::from(&t.repo);
    let timer = Timer::now();
    let timeout = Duration::from_secs(
        step.timeout_secs
            .map(u64::from)
            .unwrap_or(cfg.check_timeout_secs),
    );
    let argv: Vec<String> = match (&step.action.run, &step.action.check) {
        (Some(run), _) => run.clone(),
        (None, Some(name)) => match cfg.checks.get(name) {
            Some(argv) => argv.clone(),
            None => {
                let detail = format!("skipped: the repository declares no check named {name:?}");
                op(
                    f,
                    t.id,
                    &timer,
                    OpRow {
                        seq,
                        name: &step.action.name,
                        kernel: false,
                        ok: true,
                        exit: None,
                        detail: &detail,
                        attempt_id: None,
                        output: "",
                    },
                )?;
                return Ok((true, detail));
            }
        },
        (None, None) => {
            return Err(Fault::Task(anyhow::anyhow!(
                "operation {} has neither run nor check",
                step.action.name
            )));
        }
    };
    // HEAD before the preceding directive ran, so an operation can judge
    // that step alone: the first attempt of the last step that succeeded.
    let prev_sha = {
        let atts = f.store.attempts(t.id).env()?;
        atts.iter()
            .rev()
            .find(|a| a.state == AttemptState::Succeeded)
            .map(|last| last.step_seq)
            .and_then(|sq| atts.iter().find(|a| a.step_seq == sq))
            .map(|a| a.start_sha.clone())
            .unwrap_or_else(|| t.base_sha.clone())
    };
    let hot_files = f.store.hot_files(&t.repo, 8).env()?;
    let cache_dir = f.paths.home.join("cache");
    let _ = std::fs::create_dir_all(&cache_dir);
    let env = operation_env(t, cfg, step, &prev_sha, &hot_files, &cache_dir);
    let scratch = step
        .action
        .reads_verify_ref()
        .then(|| op_scratch_dir(&t.worktree));
    let cwd: PathBuf = match &scratch {
        Some(dir) => {
            let _ = std::fs::remove_dir_all(dir);
            git::archive_all(&repo, &t.base_sha, dir).await.task()?;
            let vref = format!("verify/{}", t.id);
            let files = git::ls_tree(&repo, &vref, &cfg.namespace).await.task()?;
            git::archive_into(&repo, &vref, &files, dir).await.task()?;
            dir.clone()
        }
        None => wt.clone(),
    };
    let start_sha = git::head(&wt).await.task()?;
    // A hidden suite: overlay the verification namespace for the run, then
    // take it away again so the next directive starts blind.
    let placed = if step.action.overlay && scratch.is_none() {
        let refs = overlay_refs(&repo, t.id, Some(&t.verify_base)).await;
        let placed = crate::verify::overlay(&repo, &refs, &cfg.namespace, &wt)
            .await
            .task()?;
        f.report.emit(
            t.id,
            Event::Note {
                text: &format!(
                    "overlay  {} file(s) from {} for {}",
                    placed.len(),
                    crate::verify::overlay_label(&refs),
                    step.action.name
                ),
            },
        );
        placed
    } else {
        Vec::new()
    };
    let r = if step.action.output_full() {
        checks::run_one_capped(
            "OP",
            &step.action.name,
            &argv,
            &cwd,
            f.sandbox.as_ref(),
            timeout,
            &env,
            checks::FULL_OUTPUT_BYTES,
        )
        .await
    } else {
        checks::run_one(
            "OP",
            &step.action.name,
            &argv,
            &cwd,
            f.sandbox.as_ref(),
            timeout,
            &env,
        )
        .await
    };
    if let Some(dir) = &scratch {
        let _ = std::fs::remove_dir_all(dir);
    }
    crate::verify::remove_overlay(&placed, &cfg.namespace, &wt);
    let detail = if r.ok {
        format!("exit 0 in {:.1}s", r.ms as f64 / 1000.0)
    } else if r.timed_out {
        format!(
            "timed out after {}s\n{}",
            timeout.as_secs(),
            checks::last_lines(&r.tail, 30)
        )
    } else {
        let tail = checks::last_lines(&r.tail, 30);
        let first = tail.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        let head = if first.is_empty() {
            format!("exit {}", r.exit.map_or("signal".into(), |c| c.to_string()))
        } else {
            first.to_string()
        };
        format!("{head}\n{tail}")
    };
    // What the operation printed is the evidence that it ran: an interface
    // operation's stdout is its product; one declaring `output = "full"`
    // keeps its whole merged stdout and stderr, capped above; any other
    // keeps its 40-line tail, pass or fail.
    let output = if r.ok && step.action.yields_interface() {
        r.stdout.trim().to_string()
    } else if step.action.output_full() {
        r.tail.trim().to_string()
    } else {
        checks::last_lines(&r.tail, 40).trim().to_string()
    };
    op(
        f,
        t.id,
        &timer,
        OpRow {
            seq,
            name: &step.action.name,
            kernel: false,
            ok: r.ok,
            exit: r.exit,
            detail: &detail,
            attempt_id: None,
            output: &output,
        },
    )?;
    if !r.ok {
        return Ok((false, detail));
    }
    if step.action.yields_context() {
        t.context = output.chars().take(12_000).collect();
        f.store.update_task(t).env()?;
        f.report.emit(
            t.id,
            Event::Note {
                text: &format!(
                    "context  {} line(s) from {}",
                    t.context.lines().count(),
                    step.action.name
                ),
            },
        );
    }
    if step.action.yields_interface() {
        t.interface = output;
        f.store.update_task(t).env()?;
        f.report.emit(
            t.id,
            Event::Note {
                text: &format!(
                    "interface {} line(s) from {}",
                    t.interface.lines().count(),
                    step.action.name
                ),
            },
        );
    }
    if step.action.mutates() {
        let timer = Timer::now();
        let committed = git::commit_all(&wt, &format!("forge: {}", step.action.name))
            .await
            .task()?;
        let Some(sha) = committed else {
            op(
                f,
                t.id,
                &timer,
                OpRow {
                    seq,
                    name: "verify",
                    kernel: true,
                    ok: true,
                    exit: None,
                    detail: "no changes; the verified tree stands",
                    attempt_id: None,
                    output: "",
                },
            )?;
            return Ok((true, detail));
        };
        f.report.emit(
            t.id,
            Event::Note {
                text: &format!("commit   {} by {}", &sha[..8], step.action.name),
            },
        );
        let overlay_refs = overlay_refs(&repo, t.id, Some(&t.verify_base)).await;
        let pending_main = git::rev_parse(&wt, &format!("refs/heads/forge/{}", t.base_branch))
            .await
            .ok();
        let v = verify::verify_operation(Subject {
            task_id: t.id,
            repo: &repo,
            worktree: &wt,
            base_sha: &t.base_sha,
            start_sha: &start_sha,
            cfg,
            task_checks: &t.checks,
            paths: &[],
            allow_protected: t.allow_protected,
            overlay_refs: &overlay_refs,
            pending_main: pending_main.as_deref(),
            sandbox: f.sandbox.as_ref(),
            report: &f.report,
            scratch: None,
        })
        .await
        .task()?;
        let ok = v.state == AttemptState::Succeeded;
        op(
            f,
            t.id,
            &timer,
            OpRow {
                seq,
                name: "verify",
                kernel: true,
                ok,
                exit: None,
                detail: &if ok {
                    format!("{} file(s) committed as {}", v.files_changed, &sha[..8])
                } else {
                    v.reason.clone()
                },
                attempt_id: None,
                output: "",
            },
        )?;
        if !ok {
            return Ok((false, v.reason));
        }
    }
    Ok((true, detail))
}

/// A deploy target's method, run outside of any task: no worktree, no
/// commit, no verify. `target`'s own arguments become `FORGE_ARG_<NAME>`
/// (uppercased) and its check command becomes `FORGE_CHECK`, both only
/// ever in this process's environment, never in a prompt and never in a
/// log (see docs/DEPLOY.md, "Secrets and hosts"). `cwd` is the landed
/// tree, already checked out by the caller.
pub(crate) async fn run_deploy_method(
    f: &Forge,
    target: &DeployTarget,
    cwd: &Path,
    timeout: Duration,
) -> anyhow::Result<checks::CheckResult> {
    let actions = workflows::load_actions(&f.paths.home)?;
    let action = actions
        .get(&target.method)
        .with_context(|| format!("unknown deploy method {:?}", target.method))?;
    let argv = action
        .run
        .as_ref()
        .with_context(|| format!("deploy method {:?} declares no run command", target.method))?;
    let mut env: Vec<(String, String)> = target
        .args
        .iter()
        .map(|(k, v)| (format!("FORGE_ARG_{}", k.to_uppercase()), v.clone()))
        .collect();
    env.push(("FORGE_CHECK".to_string(), target.check_cmd.clone()));
    Ok(checks::run_one(
        "OP",
        &action.name,
        argv,
        cwd,
        f.sandbox.as_ref(),
        timeout,
        &env,
    )
    .await)
}
