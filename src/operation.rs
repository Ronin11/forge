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
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The facts every command Forge runs on a task's tree is told, as
/// environment: an operation, a repository check (L1), a task check (L2),
/// a fix command, the tests contract's red-on-base run. One list, built
/// here and nowhere else, so a check and an operation never disagree.
/// `start_sha` is HEAD before the attempt (or the operation) began.
pub(crate) fn task_facts(
    task_id: i64,
    base_sha: &str,
    start_sha: &str,
    branch: &str,
) -> Vec<(String, String)> {
    [
        ("FORGE_TASK_ID", task_id.to_string()),
        ("FORGE_BASE_SHA", base_sha.to_string()),
        ("FORGE_START_SHA", start_sha.to_string()),
        ("FORGE_BRANCH", branch.to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// What an operation is told about its task, as environment: the task
/// facts (`task_facts`) followed by the operation's own. Facts only, each
/// one already recorded on the task.
fn operation_env(
    t: &Task,
    cfg: &config::Config,
    step: &ResolvedStep,
    start_sha: &str,
    prev_sha: &str,
    hot_files: &[String],
    cache_dir: &Path,
) -> Vec<(String, String)> {
    let mut env = task_facts(t.id, &t.base_sha, start_sha, &t.branch);
    env.extend(
        [
            ("FORGE_WORKFLOW", t.workflow.clone()),
            ("FORGE_STEP", step.action.name.clone()),
            ("FORGE_BASE_BRANCH", t.base_branch.clone()),
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
            // The map's arm from the experiment (docs/CONTEXT.md, the map
            // factor): `spans` renders `name@start-end`, `names` renders names
            // only; spans when no experiment drew it.
            (
                "FORGE_MAP_STYLE",
                t.explore
                    .get("map")
                    .cloned()
                    .unwrap_or_else(|| "spans".to_string()),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v)),
    );
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
    let start_sha = git::head(&wt).await.task()?;
    let env = operation_env(t, cfg, step, &start_sha, &prev_sha, &hot_files, &cache_dir);
    let scratch = step
        .action
        .reads_verify_ref()
        .then(|| op_scratch_dir(&t.worktree));
    let cwd: PathBuf = match &scratch {
        Some(dir) => {
            git::fresh_archive(&repo, &t.base_sha, dir).await.task()?;
            let vref = format!("verify/{}", t.id);
            let files = git::ls_tree(&repo, &vref, &cfg.namespace).await.task()?;
            git::archive_into(&repo, &vref, &files, dir).await.task()?;
            dir.clone()
        }
        None => wt.clone(),
    };
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
            branch: &t.branch,
            cfg,
            task_checks: &t.checks,
            paths: &[],
            allow_protected: t.allow_protected,
            overlay_refs: &overlay_refs,
            pending_main: pending_main.as_deref(),
            sandbox: f.sandbox.as_ref(),
            report: &f.report,
            scratch: None,
            plan_rows: true,
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

/// A job step's operation (docs/JOBS.md, "The executor"): the action's
/// `run` command, or the scratch tree's own declared check of that name.
/// Unlike a task's operation there is no worktree, no commit, no verify:
/// the exit code alone decides the step, and every fact the script needs
/// (the job id, the step, the effect log, the dry-run flag, the input, the
/// project's secrets) is already in `env`.
pub(crate) async fn run_job_operation(
    action: &workflows::ActionDef,
    repo_checks: &BTreeMap<String, Vec<String>>,
    cwd: &Path,
    env: &[(String, String)],
    timeout: Duration,
) -> anyhow::Result<checks::CheckResult> {
    let argv: Vec<String> = match (&action.run, &action.check) {
        (Some(run), _) => run.clone(),
        (None, Some(name)) => repo_checks.get(name).cloned().with_context(|| {
            format!(
                "job step {:?}: the repository declares no check named {name:?}",
                action.name
            )
        })?,
        (None, None) => {
            anyhow::bail!("job step {:?} has neither run nor check", action.name)
        }
    };
    Ok(checks::run_one("OP", &action.name, &argv, cwd, None, timeout, env).await)
}

/// An operation resolved from the operator's catalog with its run command
/// in hand, so a runner cannot be handed an action that declares none.
/// Three resolve/run pairs (the deploy method, the smoke step, the
/// provisioning) each re-did this and then `expect`ed it had been done
/// (docs/REVIEW-2.md, theme 2.2).
#[derive(Debug)]
pub(crate) struct RunAction {
    pub def: workflows::ActionDef,
    pub argv: Vec<String>,
}

/// Resolve `name` from the catalog as something runnable. `label` names it
/// in the error: `deploy method "x"`, `deploy-smoke operation`.
pub(crate) fn resolve_action(f: &Forge, name: &str, label: &str) -> anyhow::Result<RunAction> {
    let actions = workflows::load_actions(&f.paths.home)?;
    let def = actions
        .get(name)
        .with_context(|| format!("unknown {label}"))?
        .clone();
    let argv = def
        .run
        .clone()
        .with_context(|| format!("{label} declares no run command"))?;
    Ok(RunAction { def, argv })
}

/// `FORGE_ARG_<NAME>` (uppercased) for every argument: how a deploy
/// target's or a provisioning's arguments reach the operation's process,
/// only ever in that process's environment, never in a prompt and never
/// in a log (see docs/DEPLOY.md, "Secrets and hosts").
fn arg_env<'a>(args: impl IntoIterator<Item = (&'a String, &'a String)>) -> Vec<(String, String)> {
    args.into_iter()
        .map(|(k, v)| (format!("FORGE_ARG_{}", k.to_uppercase()), v.clone()))
        .collect()
}

/// Run a resolved operation outside of any task: no worktree, no commit,
/// no verify, and never sandboxed. `forge deploy` never sandboxes its
/// steps, an on-landing deploy triggered from a sandboxed task run must
/// reach the same hosts and tools the CLI does, and provisioning reaches
/// the operator's real cloud account and ssh keys.
async fn run_action(
    a: &RunAction,
    cwd: &Path,
    timeout: Duration,
    env: &[(String, String)],
) -> checks::CheckResult {
    checks::run_one("OP", &a.def.name, &a.argv, cwd, None, timeout, env).await
}

/// The action a deploy target's `method` names, resolved once up front so
/// `forge deploy` fails on an unknown method before it ever starts a
/// deploy row.
pub(crate) fn resolve_deploy_method(f: &Forge, method: &str) -> anyhow::Result<RunAction> {
    resolve_action(f, method, &format!("deploy method {method:?}"))
}

/// A deploy target's method: its arguments as `FORGE_ARG_<NAME>` and its
/// check command as `FORGE_CHECK`, the commit it deploys as
/// `FORGE_DEPLOY_SHA`, and the data directory as `FORGE_HOME` (an
/// operation's environment is otherwise cleared to the agent's); `cwd` is
/// the landed tree, already checked out by the caller.
pub(crate) async fn run_deploy_method(
    action: &RunAction,
    target: &DeployTarget,
    sha: &str,
    home: &Path,
    cwd: &Path,
    timeout: Duration,
) -> anyhow::Result<checks::CheckResult> {
    let mut env = arg_env(&target.args);
    env.push(("FORGE_CHECK".to_string(), target.check_cmd.clone()));
    env.push(("FORGE_DEPLOY_SHA".to_string(), sha.to_string()));
    env.push(("FORGE_HOME".to_string(), home.display().to_string()));
    Ok(run_action(action, cwd, timeout, &env).await)
}

/// The `deploy-smoke` operation, resolved once up front like the method,
/// so a target that declares a smoke url fails before its deploy row
/// starts if the operation is somehow missing.
pub(crate) fn resolve_deploy_smoke(f: &Forge) -> anyhow::Result<RunAction> {
    resolve_action(f, "deploy-smoke", "deploy-smoke operation")
}

/// After a deploy's check passes, open its target's smoke url in headless
/// Chromium through the `deploy-smoke` operation (see
/// src/builtins/operations/deploy-smoke.toml) and record what it saw
/// under `out_dir` (`FORGE_HOME/deploys/<id>/`; see docs/DEPLOY.md, "A
/// deterministic smoke step").
pub(crate) async fn run_deploy_smoke(
    action: &RunAction,
    url: &str,
    out_dir: &Path,
    timeout: Duration,
) -> anyhow::Result<checks::CheckResult> {
    std::fs::create_dir_all(out_dir)?;
    let env = vec![
        ("FORGE_ARG_URL".to_string(), url.to_string()),
        (
            "FORGE_ARG_OUT_DIR".to_string(),
            out_dir.display().to_string(),
        ),
    ];
    Ok(run_action(action, out_dir, timeout, &env).await)
}

/// The `provision-hetzner` operation, resolved once up front like the rest.
pub(crate) fn resolve_provision(f: &Forge) -> anyhow::Result<RunAction> {
    resolve_action(f, "provision-hetzner", "provision-hetzner operation")
}

/// `forge provision <project> <name>`: run `provision-hetzner` with
/// `args` as `FORGE_ARG_<KEY>`, plus `FORGE_ARG_OUT_DIR` for the
/// ssh-config fragment it writes (see
/// src/builtins/operations/provision-hetzner.toml).
pub(crate) async fn run_provision(
    action: &RunAction,
    args: &BTreeMap<String, String>,
    out_dir: &Path,
    timeout: Duration,
) -> anyhow::Result<checks::CheckResult> {
    std::fs::create_dir_all(out_dir)?;
    let mut env = arg_env(args);
    env.push((
        "FORGE_ARG_OUT_DIR".to_string(),
        out_dir.display().to_string(),
    ));
    Ok(run_action(action, out_dir, timeout, &env).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::Paths;
    use crate::store::Store;
    use crate::workflows::{Contract, Kind, Output, Product};

    fn test_action(name: &str, reads_verify_ref: bool) -> workflows::ActionDef {
        workflows::ActionDef {
            name: name.to_string(),
            kind: Kind::Operation,
            description: String::new(),
            consumes: if reads_verify_ref {
                vec![Product::VerifyRef]
            } else {
                vec![]
            },
            produces: vec![],
            model: None,
            max_turns: None,
            timeout_secs: None,
            run: None,
            check: None,
            contract: Contract::Code,
            paths: vec![],
            brief: String::new(),
            prompt: None,
            schema: None,
            file_into_initiative: false,
            overlay: false,
            verifies: false,
            output: Output::Tail,
            hash: String::new(),
            text: String::new(),
        }
    }

    fn test_step(action: workflows::ActionDef) -> ResolvedStep {
        ResolvedStep {
            action,
            model: None,
            max_turns: None,
            timeout_secs: None,
            via: vec![],
        }
    }

    fn test_cfg(namespace: Vec<String>) -> config::Config {
        config::Config {
            checks: BTreeMap::new(),
            fixable: BTreeMap::new(),
            base_branch: "main".into(),
            push_remote: None,
            check_timeout_secs: 60,
            protected: vec![],
            namespace,
            egress: vec![],
            config_path: "forge.toml".into(),
        }
    }

    #[test]
    fn operation_env_lists_the_exact_facts_in_order() {
        let t = Task {
            id: 42,
            workflow: "direct".into(),
            base_branch: "main".into(),
            base_sha: "abcdef0".into(),
            branch: "forge/42".into(),
            task: "do the thing".into(),
            ..Default::default()
        };
        let cfg = test_cfg(vec!["tests".into(), "src".into()]);
        let step = test_step(test_action("fmt", false));
        let hot_files = vec!["a.rs".to_string(), "b.rs".to_string()];
        let cache_dir = PathBuf::from("/tmp/forge-cache");
        let env = operation_env(
            &t,
            &cfg,
            &step,
            "start456",
            "prevsha123",
            &hot_files,
            &cache_dir,
        );

        let bin_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.display().to_string()))
            .unwrap_or_default();

        assert_eq!(
            env,
            vec![
                ("FORGE_TASK_ID".to_string(), "42".to_string()),
                ("FORGE_BASE_SHA".to_string(), "abcdef0".to_string()),
                ("FORGE_START_SHA".to_string(), "start456".to_string()),
                ("FORGE_BRANCH".to_string(), "forge/42".to_string()),
                ("FORGE_WORKFLOW".to_string(), "direct".to_string()),
                ("FORGE_STEP".to_string(), "fmt".to_string()),
                ("FORGE_BASE_BRANCH".to_string(), "main".to_string()),
                ("FORGE_NAMESPACE".to_string(), "tests src".to_string()),
                ("FORGE_PREV_SHA".to_string(), "prevsha123".to_string()),
                ("FORGE_TASK".to_string(), "do the thing".to_string()),
                ("FORGE_BIN_DIR".to_string(), bin_dir),
                ("FORGE_HOT_FILES".to_string(), "a.rs,b.rs".to_string()),
                (
                    "FORGE_CACHE_DIR".to_string(),
                    "/tmp/forge-cache".to_string()
                ),
                ("FORGE_MAP_STYLE".to_string(), "spans".to_string()),
            ]
        );
    }

    #[test]
    fn operation_env_appends_verify_ref_only_for_an_action_that_reads_it() {
        let t = Task {
            id: 7,
            ..Default::default()
        };
        let cfg = test_cfg(vec![]);
        let cache_dir = PathBuf::from("/cache");

        let reading = test_step(test_action("act", true));
        let env = operation_env(&t, &cfg, &reading, "s", "x", &[], &cache_dir);
        assert_eq!(
            env.last(),
            Some(&("FORGE_VERIFY_REF".to_string(), "verify/7".to_string()))
        );

        let not_reading = test_step(test_action("act", false));
        let env = operation_env(&t, &cfg, &not_reading, "s", "x", &[], &cache_dir);
        assert!(!env.iter().any(|(k, _)| k == "FORGE_VERIFY_REF"));
    }

    /// The facts a check is promised (docs/ACTIONS.md) are exactly the
    /// head of what an operation gets: one builder, no second list, and
    /// no key without a value.
    #[test]
    fn task_facts_has_no_gaps_and_heads_the_operation_env() {
        let facts = task_facts(9, "base1", "start2", "forge/9-x");
        let keys: Vec<&str> = facts.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "FORGE_TASK_ID",
                "FORGE_BASE_SHA",
                "FORGE_START_SHA",
                "FORGE_BRANCH"
            ]
        );
        assert!(facts.iter().all(|(_, v)| !v.is_empty()), "{facts:?}");

        let t = Task {
            id: 9,
            base_sha: "base1".into(),
            branch: "forge/9-x".into(),
            ..Default::default()
        };
        let cfg = test_cfg(vec![]);
        let step = test_step(test_action("act", false));
        let env = operation_env(&t, &cfg, &step, "start2", "x", &[], &PathBuf::from("/c"));
        assert_eq!(&env[..facts.len()], &facts[..]);
    }

    #[test]
    fn arg_env_uppercases_each_name_and_keeps_input_order() {
        let zeta = ("zeta".to_string(), "z".to_string());
        let alpha = ("alpha".to_string(), "a".to_string());
        let args = vec![(&zeta.0, &zeta.1), (&alpha.0, &alpha.1)];

        let env = arg_env(args);

        assert_eq!(
            env,
            vec![
                ("FORGE_ARG_ZETA".to_string(), "z".to_string()),
                ("FORGE_ARG_ALPHA".to_string(), "a".to_string()),
            ]
        );
    }

    #[test]
    fn op_scratch_dir_appends_op_to_the_worktree_path() {
        assert_eq!(
            op_scratch_dir("/home/x/worktrees/42"),
            PathBuf::from("/home/x/worktrees/42-op")
        );
    }

    /// A `Forge` over a fresh, empty store in a throwaway home: enough for
    /// `resolve_action` to build its catalog directory (built-ins written
    /// on first use, see `workflows::ensure`).
    fn fixture() -> (tempfile::TempDir, Forge) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let paths = Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = Store::open(&paths.home.join("forge.db")).unwrap();
        let f = Forge::open_with(paths, store).unwrap();
        (dir, f)
    }

    #[test]
    fn resolve_action_errors_on_an_unknown_name_naming_the_label() {
        let (_dir, f) = fixture();
        let err =
            resolve_action(&f, "no-such-action", "deploy method \"no-such-action\"").unwrap_err();
        assert_eq!(err.to_string(), "unknown deploy method \"no-such-action\"");
    }

    #[test]
    fn resolve_action_errors_when_the_action_declares_no_run_command() {
        let (_dir, f) = fixture();
        // "code" is a built-in directive: it never declares `run`.
        let err = resolve_action(&f, "code", "deploy method \"code\"").unwrap_err();
        assert_eq!(
            err.to_string(),
            "deploy method \"code\" declares no run command"
        );
    }

    #[test]
    fn resolve_action_returns_the_argv_of_a_runnable_action() {
        let (_dir, f) = fixture();
        // "fmt" is a built-in operation with a `run` command.
        let a = resolve_action(&f, "fmt", "fmt operation").unwrap();
        assert_eq!(a.def.name, "fmt");
        assert_eq!(Some(a.argv), a.def.run);
    }
}
