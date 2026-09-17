//! `forge job start`: the executor for operation-only run workflows, run
//! inline with `--now` (docs/JOBS.md, "The executor"). Without `--now` the
//! job is only recorded as `queued`; claiming a queued job is the worker's
//! job, a later build-order step.
//!
//! A run's steps see no worktree, no clone, no commit: the project's first
//! repository is materialised at its latest landed commit into a scratch
//! directory (an archive, the way a deploy's is), the trigger's input is
//! written there as files and environment, the project's secrets are
//! injected as environment, and each step's operation runs through
//! `operation::run_job_operation` in order, appending to an effect log the
//! executor reads back after every step. The `[assert]` commands then
//! judge the run with that log and the scratch directory on disk. Unlike a
//! deploy's, the scratch and input directories are left on disk after the
//! run: `job_steps.output_ref` points into the scratch tree, and a dry
//! run is proven by what is (and is not) there.

use crate::ctx::Forge;
use crate::store::{Job, JobEffect, JobState, JobStep};
use crate::workflows::{self, Kind};
use crate::{checks, config, git, operation, unix_now};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn scratch_dir(f: &Forge, job_id: i64) -> PathBuf {
    f.paths.worktrees.join(format!("job-{job_id}"))
}

fn input_dir(f: &Forge, job_id: i64) -> PathBuf {
    f.paths.worktrees.join(format!("job-{job_id}-input"))
}

/// Every top-level field of `input` whose value is a string, as
/// `(name, value)` pairs — what becomes `FORGE_INPUT_<NAME>`.
fn string_fields(input: &serde_json::Value) -> Result<Vec<(String, String)>> {
    let obj = input
        .as_object()
        .context("the input file must hold a JSON object")?;
    Ok(obj
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect())
}

/// The environment every job step's operation runs with: the facts the
/// docs promise (docs/JOBS.md, "The executor") — never more, and never a
/// secret logged or put in a prompt.
fn step_env(
    job_id: i64,
    step_name: &str,
    effect_log: &Path,
    input_dir: &Path,
    input_fields: &[(String, String)],
    secrets: &std::collections::BTreeMap<String, String>,
    dry_run: bool,
) -> Vec<(String, String)> {
    let mut env = vec![
        ("FORGE_JOB_ID".to_string(), job_id.to_string()),
        ("FORGE_STEP".to_string(), step_name.to_string()),
        (
            "FORGE_EFFECT_LOG".to_string(),
            effect_log.display().to_string(),
        ),
        (
            "FORGE_INPUT_DIR".to_string(),
            input_dir.display().to_string(),
        ),
    ];
    if dry_run {
        env.push(("FORGE_DRY_RUN".to_string(), "1".to_string()));
    }
    for (k, v) in input_fields {
        env.push((format!("FORGE_INPUT_{}", k.to_uppercase()), v.clone()));
    }
    for (k, v) in secrets {
        env.push((k.clone(), v.clone()));
    }
    env
}

/// Lines an effect log holds right now; a fresh log (or one not yet
/// created) holds none.
fn log_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// `forge job start <project> <workflow>`: record a job and, with `--now`,
/// run it in this process. Returns the job's id.
#[allow(clippy::too_many_arguments)]
pub async fn start(
    f: &Forge,
    project: &str,
    workflow: &str,
    input: Option<&Path>,
    dry_run: bool,
    now: bool,
) -> Result<i64> {
    f.store
        .project(project)?
        .with_context(|| format!("no project {project}"))?;
    let (wf, steps) = workflows::resolve_job(&f.paths.home, workflow)?;
    if let Some(a) = steps.iter().find(|a| a.kind == Kind::Directive) {
        bail!(
            "job step {:?} is a directive; directive steps are not supported yet (docs/JOBS.md, build order step 2)",
            a.name
        );
    }
    let repo = f
        .store
        .first_repo(project)?
        .with_context(|| format!("project {project} has no registered repository"))?;
    let repo_path = PathBuf::from(&repo);
    let cfg = config::load_working(&repo_path).await?;
    let landed_sha = git::rev_parse(&repo_path, &format!("refs/heads/{}", cfg.base_branch))
        .await
        .with_context(|| format!("resolving {} on {}", cfg.base_branch, repo_path.display()))?;

    let input_text = match input {
        Some(p) => {
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?
        }
        None => "{}".to_string(),
    };
    let input_json: serde_json::Value =
        serde_json::from_str(&input_text).context("parsing the input file as JSON")?;
    let input_fields = string_fields(&input_json)?;

    let started_at = unix_now();
    let job = Job {
        id: 0,
        project: project.to_string(),
        workflow: workflow.to_string(),
        workflow_hash: wf.hash.clone(),
        landed_sha: landed_sha.clone(),
        trigger_kind: workflows::TriggerOn::Manual.as_str().to_string(),
        trigger_ref: String::new(),
        state: if now {
            JobState::Running
        } else {
            JobState::Queued
        },
        dry_run,
        started_at,
        finished_at: None,
        cost_usd: None,
        verdict_json: "[]".to_string(),
    };
    let job_id = f.store.create_job(&job)?;
    if !now {
        return Ok(job_id);
    }

    run_now(
        f,
        job_id,
        project,
        &repo_path,
        &landed_sha,
        &steps,
        &wf.assert,
        wf.limits.as_ref(),
        dry_run,
        &input_text,
        &input_fields,
        cfg.check_timeout_secs,
    )
    .await?;
    Ok(job_id)
}

/// Run a job's steps and assertions now, recording everything as it goes,
/// and `finish_job` with the final state, cost and verdict.
#[allow(clippy::too_many_arguments)]
async fn run_now(
    f: &Forge,
    job_id: i64,
    project: &str,
    repo: &Path,
    landed_sha: &str,
    steps: &[workflows::ActionDef],
    assert: &std::collections::BTreeMap<String, Vec<String>>,
    limits: Option<&workflows::Limits>,
    dry_run: bool,
    input_text: &str,
    input_fields: &[(String, String)],
    check_timeout_secs: u64,
) -> Result<()> {
    let scratch = scratch_dir(f, job_id);
    let _ = std::fs::remove_dir_all(&scratch);
    git::archive_all(repo, landed_sha, &scratch).await?;
    let repo_checks = config::load_working(&scratch)
        .await
        .map(|c| c.checks)
        .unwrap_or_default();

    let idir = input_dir(f, job_id);
    let _ = std::fs::remove_dir_all(&idir);
    std::fs::create_dir_all(&idir)?;
    std::fs::write(idir.join("input.json"), input_text)?;

    let effect_log = idir.join("effects.log");
    std::fs::write(&effect_log, "")?;

    let secrets = f.project_secrets.get(project).cloned().unwrap_or_default();
    let timeout = Duration::from_secs(check_timeout_secs);

    let mut ok = true;
    let mut verdict: Vec<checks::CheckResult> = Vec::new();
    for (seq, action) in steps.iter().enumerate() {
        let seq = seq as i64;
        let before = log_lines(&effect_log).len();
        let env = step_env(
            job_id,
            &action.name,
            &effect_log,
            &idir,
            input_fields,
            &secrets,
            dry_run,
        );
        let started_at = unix_now();
        let r = match operation::run_job_operation(action, &repo_checks, &scratch, &env, timeout)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                ok = false;
                verdict.push(checks::CheckResult {
                    level: "OP".to_string(),
                    name: action.name.clone(),
                    ok: false,
                    tail: format!("{e:#}"),
                    ..Default::default()
                });
                break;
            }
        };
        f.store.append_job_step(&JobStep {
            id: 0,
            job_id,
            seq,
            action: action.name.clone(),
            kind: "operation".to_string(),
            provider: String::new(),
            model: String::new(),
            cost_usd: Some(0.0),
            started_at,
            finished_at: Some(unix_now()),
            exit_code: r.exit,
            output_ref: String::new(),
        })?;
        for line in log_lines(&effect_log).into_iter().skip(before) {
            let mut parts = line.splitn(3, '\t');
            let (Some(kind), Some(target), Some(summary)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            f.store.append_job_effect(&JobEffect {
                id: 0,
                job_id,
                seq,
                kind: kind.to_string(),
                target: target.to_string(),
                summary: summary.to_string(),
                dry_run,
            })?;
        }
        if !r.ok {
            ok = false;
            break;
        }
    }

    if ok {
        for (name, argv) in assert {
            let env = vec![
                (
                    "FORGE_EFFECT_LOG".to_string(),
                    effect_log.display().to_string(),
                ),
                (
                    "FORGE_SCRATCH_DIR".to_string(),
                    scratch.display().to_string(),
                ),
            ];
            let r = checks::run_one("L0", name, argv, &scratch, None, timeout, &env).await;
            if !r.ok {
                ok = false;
            }
            verdict.push(r);
        }
    }
    // Operations cost nothing; the budget starts to matter once a directive
    // step can (docs/JOBS.md, build order step 2).
    let cost_usd = 0.0;
    if let Some(l) = limits {
        let within = cost_usd <= l.budget_usd;
        ok = ok && within;
        verdict.push(checks::CheckResult {
            level: "L0".to_string(),
            name: "budget".to_string(),
            ok: within,
            tail: format!("${cost_usd:.2} of ${:.2}", l.budget_usd),
            ..Default::default()
        });
    }

    let state = if ok { JobState::Ok } else { JobState::Failed };
    f.store.finish_job(
        job_id,
        unix_now(),
        state,
        Some(cost_usd),
        &serde_json::to_string(&verdict)?,
    )?;
    Ok(())
}
