//! `forge job start`: the executor for operation-only run workflows, run
//! inline with `--now` (docs/JOBS.md, "The executor"). Without `--now` the
//! job is only recorded as `queued`; `drive` is what the worker's claim
//! loop (`src/worker.rs`) calls to run it, later, the same way.
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
//! run: a dry run is proven by what is (and is not) there. An operation
//! step keeps the last lines of its stdout and stderr on its `job_steps`
//! row (`tail`), and all it printed in a file under the input directory
//! that `output_ref` names.

/// Operator-supplied workflow, input path, and scheduling options for a new job.
pub struct Start<'a> {
    pub f: &'a Forge,
    pub project: &'a str,
    pub workflow: &'a str,
    pub input: Option<&'a Path>,
    pub dry_run: bool,
    pub now: bool,
    pub due_at: Option<i64>,
}

/// Job inputs, prior outputs, and configuration exposed to a workflow step.
struct StepEnv<'a> {
    job_id: i64,
    step_name: &'a str,
    effect_log: &'a Path,
    input_dir: &'a Path,
    input_fields: &'a [(String, String)],
    output_paths: &'a [(String, String)],
    project: &'a str,
    repo: &'a Path,
    home: &'a Path,
    workflow_env: &'a BTreeMap<String, String>,
    secrets: &'a std::collections::BTreeMap<String, String>,
    dry_run: bool,
}

/// A landed event and workflow used to enqueue a deduplicated job.
pub struct StartEvent<'a> {
    pub f: &'a Forge,
    pub project: &'a str,
    pub workflow: &'a str,
    pub landed_sha: &'a str,
    pub wf: &'a workflows::Workflow,
    pub source: workflows::JobSource,
    pub offset: &'a str,
    pub at: i64,
    pub input: &'a str,
}

/// A webhook delivery and workflow used to enqueue a deduplicated job.
pub struct StartWebhook<'a> {
    pub f: &'a Forge,
    pub project: &'a str,
    pub workflow: &'a str,
    pub landed_sha: &'a str,
    pub wf: &'a workflows::Workflow,
    pub source: workflows::JobSource,
    pub trigger_ref: &'a str,
    pub input_text: &'a str,
    /// The trust level of the token the delivery carried (`forge job fire`).
    pub trust: Trust,
}

/// Trigger identity, delivery time, and input payload for a queued job.
struct QueueTriggered<'a> {
    f: &'a Forge,
    project: &'a str,
    workflow: &'a str,
    landed_sha: &'a str,
    wf: &'a workflows::Workflow,
    source: workflows::JobSource,
    kind: workflows::TriggerOn,
    trigger_ref: &'a str,
    event_at: i64,
    input_text: &'a str,
    /// The trust level this job is recorded at (`store::set_job_trust`),
    /// before it can be claimed: what `secrets::step_grant` reads back to
    /// decide whether a step's secrets and egress are granted.
    trust: Trust,
}

/// Resolved workflow, inputs, and recorded outputs for an inline job run.
struct RunNow<'a> {
    f: &'a Forge,
    job_id: i64,
    project: &'a str,
    workflow: &'a str,
    repo: &'a Path,
    landed_sha: &'a str,
    steps: &'a [workflows::RunStep],
    assert: &'a BTreeMap<String, Vec<String>>,
    skip_if: &'a BTreeMap<String, Vec<String>>,
    limits: Option<&'a workflows::Limits>,
    trigger: Option<&'a workflows::Trigger>,
    workflow_env: &'a BTreeMap<String, String>,
    dry_run: bool,
    input_text: &'a str,
    input_fields: &'a [(String, String)],
    check_timeout_secs: u64,
    project_roles: &'a BTreeMap<String, String>,
    recorded: &'a BTreeMap<String, serde_json::Value>,
}

mod directive;
mod directive_text;
mod failure;
mod fixture;
mod flow;
mod input;
mod operation_step;
mod recovery;
#[cfg(test)]
mod recovery_tests;
use directive::{RunDirective, recorded_directive, run_directive};
use failure::{FailureAction, ask, decide_on_failure, failure_reason, retry_job};
pub(crate) use failure::{question_job_id, trigger_contact};
pub use fixture::{bench, require_fixture_pass, test};

use crate::ctx::Forge;
use crate::report::Event;
use crate::store::{Job, JobEffect, JobState, JobStep, Message, Owner, Trust};
use crate::workflows::{self, Kind};
use crate::{checks, config, git, unix_now};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn scratch_dir(f: &Forge, job_id: i64) -> PathBuf {
    f.paths.worktrees.join(format!("job-{job_id}"))
}

pub(crate) fn input_dir(f: &Forge, job_id: i64) -> PathBuf {
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

/// How many trailing lines of an operation's merged stdout and stderr a
/// job step's row keeps (its `tail`), and so what a failed step's verdict
/// row and the human rung's question quote.
const STEP_TAIL_LINES: usize = 20;

/// What an operation step leaves of its output: the last lines on the row
/// (`tail`), and the whole of what was kept in a file under the job's input
/// directory that `output_ref` names — whatever the step's exit, so the
/// record of a step is never empty for want of a check that failed.
fn record_output(idir: &Path, label: &str, r: &checks::CheckResult) -> (String, String) {
    let tail = checks::last_lines(&r.tail, STEP_TAIL_LINES);
    let path = idir.join(format!("step-{label}.out"));
    let output_ref = match std::fs::write(&path, &r.tail) {
        Ok(()) => path.display().to_string(),
        Err(_) => String::new(),
    };
    (tail, output_ref)
}

/// The environment every job step's operation runs with: the facts the
/// docs promise (docs/JOBS.md, "The executor") — never more, and never a
/// secret logged or put in a prompt. `output_paths` names, by action name,
/// where an earlier directive step's validated output landed
/// (`FORGE_OUTPUT_<NAME>`): how a later operation reads what a directive
/// decided (docs/JOBS.md, "The executor", item 3: "Outputs are files in
/// the scratch directory and flow to the next step"). `FORGE_PROJECT` and
/// `FORGE_REPO_DIR` name the real, landed repository (with its `.git`,
/// unlike the scratch archive the step runs in) so a step that has to act
/// on the project itself — filing a task with `forge add`, say — knows
/// where; `FORGE_BIN_DIR` is where that `forge` binary lives, the fact
/// `operation::operation_env` already gives a task's own operations.
/// `FORGE_HOME` is set for the same reason: `agent::command_in` clears
/// the child's environment down to an allowlist that does not include it
/// (so an operation never inherits stray operator state by accident), and
/// without it a recursive `forge` call would open a default store instead
/// of the operator's own. `workflow_env` is the workflow's own `[env]`
/// table: thresholds and the like, declared in the file instead of
/// hard-coded in the script.
fn step_env(args: StepEnv<'_>) -> Vec<(String, String)> {
    let StepEnv {
        job_id,
        step_name,
        effect_log,
        input_dir,
        input_fields,
        output_paths,
        project,
        repo,
        home,
        workflow_env,
        secrets,
        dry_run,
    } = args;
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
        ("FORGE_PROJECT".to_string(), project.to_string()),
        ("FORGE_REPO_DIR".to_string(), repo.display().to_string()),
        ("FORGE_HOME".to_string(), home.display().to_string()),
        (
            "FORGE_BIN_DIR".to_string(),
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.display().to_string()))
                .unwrap_or_default(),
        ),
    ];
    if dry_run {
        env.push(("FORGE_DRY_RUN".to_string(), "1".to_string()));
    }
    for (k, v) in input_fields {
        env.push((format!("FORGE_INPUT_{}", k.to_uppercase()), v.clone()));
    }
    for (name, path) in output_paths {
        env.push((
            format!("FORGE_OUTPUT_{}", name.to_uppercase().replace('-', "_")),
            path.clone(),
        ));
    }
    for (k, v) in workflow_env {
        env.push((k.clone(), v.clone()));
    }
    for (k, v) in secrets {
        env.push((k.clone(), v.clone()));
    }
    env
}

/// The verdict row of a run whose spend passed its `[limits] budget` at
/// `action`, which is a question for the operator rather than a failure.
fn over_budget(action: &str, total_cost: f64, budget_usd: f64) -> checks::CheckResult {
    checks::CheckResult {
        level: "L0".to_string(),
        name: "budget".to_string(),
        ok: false,
        tail: format!(
            "step {action} brought the run to ${total_cost:.4}, over the ${budget_usd:.2} \
             per-run budget; asking the operator"
        ),
        ..Default::default()
    }
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

/// Bounds `text` to `limit` bytes on a char boundary, noting the cut so a
/// directive is never left wondering whether it saw the whole thing.
fn bounded(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut cut = limit;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n... [inputs cut to {limit} bytes]", &text[..cut])
}

/// The state a delayed job starts in (docs/JOBS.md, "Delayed jobs"):
/// `Scheduled` while `due_at` is still ahead of `at` (the moment the job is
/// created), `Queued` once it is already due — `--delay 0s`, or a
/// `[trigger] delay` a slow-firing tick already outran. The wait, when
/// there is one, is this row's `due_at` alone; `claim_next_job` is the only
/// place that ever compares it to now again.
fn scheduled_state(due_at: Option<i64>, at: i64) -> JobState {
    if due_at.is_some_and(|d| d > at) {
        JobState::Scheduled
    } else {
        JobState::Queued
    }
}

/// `forge job start <project> <workflow>`: record a job and, with `--now`,
/// run it in this process. `due_at`, from `--at`/`--delay`, leaves it
/// `Scheduled` until then instead of `Queued` (docs/JOBS.md, "Delayed
/// jobs"); refused together with `now`, which runs inline immediately.
/// Returns the job's id.
pub async fn start(args: Start<'_>) -> Result<i64> {
    let Start {
        f,
        project,
        workflow,
        input,
        dry_run,
        now,
        due_at,
    } = args;
    if now && due_at.is_some() {
        anyhow::bail!("--now runs inline immediately; it cannot be combined with --at or --delay");
    }
    let project_row = f
        .store
        .project(project)?
        .with_context(|| format!("no project {project}"))?;
    let repo = f
        .store
        .first_repo(project)?
        .with_context(|| format!("project {project} has no registered repository"))?;
    let repo_path = PathBuf::from(&repo);
    let cfg = config::load_working(&repo_path).await?;
    let landed_sha = git::rev_parse(&repo_path, &format!("refs/heads/{}", cfg.base_branch))
        .await
        .with_context(|| format!("resolving {} on {}", cfg.base_branch, repo_path.display()))?;
    let (wf, steps, source) =
        workflows::resolve_job_for_project(&f.paths.home, &repo_path, &landed_sha, workflow)?;

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
            JobState::Scheduled
        },
        workflow_source: source.as_str().to_string(),
        dry_run,
        started_at,
        finished_at: None,
        cost_usd: None,
        verdict_json: "[]".to_string(),
        due_at: None,
        retry_count: 0,
    };
    let job_id = match wf.limits.as_ref().filter(|l| l.per_day > 0 && !dry_run) {
        Some(l) => f.store.create_job_within(
            &job,
            i64::from(l.per_day),
            started_at - 24 * 3600,
            "; it can start again when the oldest of those is a day old (asking instead is docs/JOBS.md step 5)",
        )?,
        None => f.store.create_job(&job)?,
    };
    f.store.set_job_trust(job_id, Trust::Operator)?;
    if !now {
        input::publish(f, job_id, &input_text, due_at)?;
        return Ok(job_id);
    }

    run_now(RunNow {
        f,
        job_id,
        project,
        workflow,
        repo: &repo_path,
        landed_sha: &landed_sha,
        steps: &steps,
        assert: &wf.assert,
        skip_if: &wf.skip_if,
        limits: wf.limits.as_ref(),
        trigger: wf.trigger.as_ref(),
        workflow_env: &wf.env,
        dry_run,
        input_text: &input_text,
        input_fields: &input_fields,
        check_timeout_secs: cfg.check_timeout_secs,
        project_roles: &project_row.role_providers,
        recorded: &BTreeMap::new(),
    })
    .await?;
    Ok(job_id)
}

/// Start a job the worker's schedule tick (`src/worker.rs`) found due
/// (docs/JOBS.md, "Triggers"): never run inline, with `trigger_kind =
/// "schedule"` and `trigger_ref` the due slot's unix second — the mark
/// `store::last_scheduled_job` reads back so the same slot is never
/// started twice. Shares `start`'s `per_day` accounting, so a schedule
/// obeys the same cap a manual trigger does. The workflow's own `[trigger]
/// delay` (docs/JOBS.md, "Delayed jobs"), if any, is added to `slot` — the
/// firing's own event time, not the moment the tick happens to run — to
/// get `due_at`; the job is `Queued` when there is none, or the delay has
/// already elapsed, and `Scheduled` otherwise.
pub async fn start_scheduled(
    f: &Forge,
    project: &str,
    workflow: &str,
    landed_sha: &str,
    wf: &workflows::Workflow,
    source: workflows::JobSource,
    slot: i64,
) -> Result<i64> {
    if f.store.last_scheduled_job(project, workflow)?.is_none() {
        require_fixture_pass(f, project, workflow, landed_sha).await?;
    }
    queue_triggered(QueueTriggered {
        f,
        project,
        workflow,
        landed_sha,
        wf,
        source,
        kind: workflows::TriggerOn::Schedule,
        trigger_ref: &slot.to_string(),
        event_at: slot,
        input_text: "{}",
        trust: Trust::Operator,
    })
}

/// The input a message trigger gives its job (docs/JOBS.md, "Triggers"):
/// who said it, what, when, on which channel, and the record's own id.
/// Only the string fields become `FORGE_INPUT_*`; `at` and `message_id` are
/// read from `input.json` in `FORGE_INPUT_DIR`.
fn message_input(m: &Message) -> serde_json::Value {
    serde_json::json!({
        "from": m.contact,
        "text": m.text,
        "at": m.at,
        "channel": m.channel,
        "message_id": m.id,
    })
}

/// Start a job for one inbound message a run workflow's `[trigger] on =
/// "message"` matched (`worker::message_triggers`): queued, never run
/// inline, `trigger_kind = "message"` and `trigger_ref` the message's id.
/// `None` when this workflow already started a job for this message, so
/// recording it twice starts one job, not two; `[trigger] delay` is added
/// to the message's own `at` the way a schedule's is added to its slot.
pub fn start_message(
    f: &Forge,
    project: &str,
    workflow: &str,
    landed_sha: &str,
    wf: &workflows::Workflow,
    source: workflows::JobSource,
    m: &Message,
) -> Result<Option<i64>> {
    let kind = workflows::TriggerOn::Message;
    let trigger_ref = m.id.to_string();
    if f.store
        .job_for_trigger(project, workflow, kind.as_str(), &trigger_ref)?
        .is_some()
    {
        return Ok(None);
    }
    queue_triggered(QueueTriggered {
        f,
        project,
        workflow,
        landed_sha,
        wf,
        source,
        kind,
        trigger_ref: &trigger_ref,
        event_at: m.at,
        input_text: &message_input(m).to_string(),
        trust: Trust::Contact,
    })
    .map(Some)
}

/// Start a job for one Forge event a run workflow's `[trigger] on =
/// "event"` matched (`worker::event_tick`): queued, never run inline,
/// `trigger_kind = "event"` and `trigger_ref` the event's generation:offset in
/// `events.jsonl`, `input` the event's own JSON line. `None` when this
/// workflow already started a job for that offset, so an event the tick
/// examines twice starts one job; `[trigger] delay` is added to the event's
/// own time (`at`).
pub fn start_event(args: StartEvent<'_>) -> Result<Option<i64>> {
    let StartEvent {
        f,
        project,
        workflow,
        landed_sha,
        wf,
        source,
        offset,
        at,
        input,
    } = args;
    let kind = workflows::TriggerOn::Event;
    let trigger_ref = offset.to_string();
    let earlier = || {
        f.store
            .job_for_trigger(project, workflow, kind.as_str(), &trigger_ref)
    };
    if earlier()?.is_some() {
        return Ok(None);
    }
    match queue_triggered(QueueTriggered {
        f,
        project,
        workflow,
        landed_sha,
        wf,
        source,
        kind,
        trigger_ref: &trigger_ref,
        event_at: at,
        input_text: input,
        trust: Trust::Operator,
    }) {
        Ok(id) => Ok(Some(id)),
        // Two ticks racing on one event: the unique index refused the
        // second, and the first's job is the one that stands.
        Err(e) => match earlier() {
            Ok(Some(_)) => Ok(None),
            _ => Err(e),
        },
    }
}

/// The lowercase hex SHA-256 of `bytes`: a webhook token's stored form and
/// the default key of a delivery that names none.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Start a job for one webhook delivery (`worker::webhook_workflow` found
/// the run workflow): queued, never run inline, `trigger_kind = "webhook"`
/// and `trigger_ref` the delivery's key — the caller's `--ref`, else the
/// SHA-256 of the input — so a delivery its sender retries starts one job.
/// Returns the job's id and whether this call started it: `false` for the
/// job an earlier delivery of the same key already started, which is
/// returned rather than refused, since a retry is the sender doing its
/// job. The input file's whole text is the job's `input.json`, as `forge
/// job start --input` would leave it, and must be a JSON object (an empty
/// file is `{}`); `[trigger] delay` is added to the moment of delivery.
pub fn start_webhook(args: StartWebhook<'_>) -> Result<(i64, bool)> {
    let StartWebhook {
        f,
        project,
        workflow,
        landed_sha,
        wf,
        source,
        trigger_ref,
        input_text,
        trust,
    } = args;
    let kind = workflows::TriggerOn::Webhook;
    let input_text = if input_text.trim().is_empty() {
        "{}"
    } else {
        input_text
    };
    let input_json: serde_json::Value =
        serde_json::from_str(input_text).context("parsing the input file as JSON")?;
    string_fields(&input_json)?;
    let earlier = || {
        f.store
            .job_for_trigger(project, workflow, kind.as_str(), trigger_ref)
    };
    if let Some(id) = earlier()? {
        return Ok((id, false));
    }
    match queue_triggered(QueueTriggered {
        f,
        project,
        workflow,
        landed_sha,
        wf,
        source,
        kind,
        trigger_ref,
        event_at: unix_now(),
        input_text,
        trust,
    }) {
        Ok(id) => Ok((id, true)),
        // Two deliveries of one key racing: the unique index refused the
        // second, and the first's job is the one to report.
        Err(e) => match earlier() {
            Ok(Some(id)) => Ok((id, false)),
            _ => Err(e),
        },
    }
}

/// Record a job a trigger (not `forge job start`) fired, queued for the
/// worker: `per_day` checked like a manual start, `due_at` the firing's own
/// `event_at` plus the trigger's `delay`, `input_text` written where the
/// worker's claim reads it.
fn queue_triggered(args: QueueTriggered<'_>) -> Result<i64> {
    let QueueTriggered {
        f,
        project,
        workflow,
        landed_sha,
        wf,
        source,
        kind,
        trigger_ref,
        event_at,
        input_text,
        trust,
    } = args;
    let started_at = unix_now();
    let due_at = wf
        .trigger
        .as_ref()
        .and_then(|t| t.delay)
        .map(|d| event_at + d);
    let job = Job {
        id: 0,
        project: project.to_string(),
        workflow: workflow.to_string(),
        workflow_hash: wf.hash.clone(),
        landed_sha: landed_sha.to_string(),
        trigger_kind: kind.as_str().to_string(),
        trigger_ref: trigger_ref.to_string(),
        state: JobState::Scheduled,
        workflow_source: source.as_str().to_string(),
        dry_run: false,
        started_at,
        finished_at: None,
        cost_usd: None,
        verdict_json: "[]".to_string(),
        due_at: None,
        retry_count: 0,
    };
    let job_id = match wf.limits.as_ref().filter(|l| l.per_day > 0) {
        Some(l) => {
            f.store
                .create_job_within(&job, i64::from(l.per_day), started_at - 24 * 3600, "")?
        }
        None => f.store.create_job(&job)?,
    };
    f.store.set_job_trust(job_id, trust)?;
    input::publish(f, job_id, input_text, due_at)?;
    Ok(job_id)
}

/// Run a job's `[skip_if]` commands, then its steps and assertions,
/// recording everything as it goes, and `finish_job` with the final state,
/// cost and verdict. Emits `JobStarted` on entry and `JobFinished` once
/// `finish_job` is recorded (docs/JOBS.md, "The executor" and "Skipping a
/// run"): the one place both `--now`, the worker's claimed run
/// (`run_claimed`, called by `drive`) and a fixture replay (`test`) funnel
/// through. `recorded` maps a directive step's action to the output that
/// stands in for its model call; only a replay passes any.
async fn run_now(args: RunNow<'_>) -> Result<()> {
    let RunNow {
        f,
        job_id,
        project,
        workflow,
        repo,
        landed_sha,
        steps,
        assert,
        skip_if,
        limits,
        trigger,
        workflow_env,
        dry_run,
        input_text,
        input_fields,
        check_timeout_secs,
        project_roles,
        recorded,
    } = args;
    f.report.emit(
        0,
        Event::JobStarted {
            project,
            workflow,
            job_id,
            dry_run,
        },
    );
    let scratch = scratch_dir(f, job_id);
    git::fresh_archive(repo, landed_sha, &scratch).await?;
    let repo_checks = config::load_working_checks(&scratch).unwrap_or_default();

    let idir = recovery::prepare_run(f, job_id, input_text)?;

    let effect_log = idir.join("effects.log");
    std::fs::write(&effect_log, "")?;

    let secrets = f.project_secrets.get(project).cloned().unwrap_or_default();
    let trust = f.store.job_trust(job_id)?.unwrap_or(Trust::Public);
    let timeout = Duration::from_secs(check_timeout_secs);
    let input_bytes = limits.map_or(workflows::default_input_bytes(), |l| l.input_bytes);

    let mut ok = true;
    let mut needs_human = false;
    let mut verdict: Vec<checks::CheckResult> = Vec::new();
    let mut step_outputs: Vec<(String, String)> = Vec::new();
    let mut output_paths: Vec<(String, String)> = Vec::new();
    let mut total_cost = f.store.job_step_cost(job_id)?;

    // The repository's declared `setup` check, once, in the scratch tree
    // before `[skip_if]` and the steps, the way a task's clone has it run
    // before its steps: a step that needs the repository's dependencies
    // finds them (docs/JOBS.md, "The executor"). A repository declaring
    // none runs as it always did, and so does a workflow of directive steps
    // only, which never touch the scratch tree's dependencies. It is a job
    // step row of its own, ahead of step 0; a failure ends the job `Failed`
    // with a `setup` verdict row carrying its tail, before anything else.
    let wants_setup = steps.iter().any(|s| s.action.kind == Kind::Operation);
    if wants_setup && let Some(argv) = repo_checks.get("setup") {
        let env = step_env(StepEnv {
            job_id,
            step_name: "setup",
            effect_log: &effect_log,
            input_dir: &idir,
            input_fields,
            output_paths: &[],
            project,
            repo,
            home: &f.paths.home,
            workflow_env,
            secrets: &secrets,
            dry_run,
        });
        let started_at = unix_now();
        let r = checks::run_one("OP", "setup", argv, &scratch, None, timeout, &env).await;
        let (tail, output_ref) = record_output(&idir, "setup", &r);
        f.store.append_job_step(&JobStep {
            run: 0,
            id: 0,
            job_id,
            seq: -1,
            action: "setup".to_string(),
            kind: "operation".to_string(),
            provider: String::new(),
            model: String::new(),
            cost_usd: Some(0.0),
            started_at,
            finished_at: Some(unix_now()),
            exit_code: r.exit,
            output_ref,
            tail: tail.clone(),
            outcome: String::new(),
            probabilities: String::new(),
            node: String::new(),
        })?;
        if !r.ok {
            ok = false;
            verdict.push(checks::CheckResult { tail, ..r });
        }
    }

    let setup_ok = ok;

    // `[skip_if]`, in name order: the first command to exit 0 ends the job
    // `Skipped` before any step runs, counting against nothing — not
    // `per_day`, not `on_failure`, not the failed rollup (docs/JOBS.md,
    // "Skipping a run"). A non-zero exit means "not skipped, proceed".
    for (name, argv) in skip_if.iter().filter(|_| setup_ok) {
        let env = step_env(StepEnv {
            job_id,
            step_name: name,
            effect_log: &effect_log,
            input_dir: &idir,
            input_fields,
            output_paths: &[],
            project,
            repo,
            home: &f.paths.home,
            workflow_env,
            secrets: &secrets,
            dry_run,
        });
        let r = checks::run_one("L0", name, argv, &scratch, None, timeout, &env).await;
        if r.ok {
            let reason = r.stdout.lines().next().unwrap_or_default().to_string();
            let verdict = vec![checks::CheckResult {
                level: "L0".to_string(),
                name: name.clone(),
                ok: true,
                tail: reason,
                ..Default::default()
            }];
            f.store.finish_job(
                job_id,
                unix_now(),
                JobState::Skipped,
                Some(0.0),
                &serde_json::to_string(&verdict)?,
            )?;
            f.report.emit(
                0,
                Event::JobFinished {
                    project,
                    workflow,
                    job_id,
                    state: JobState::Skipped.as_str(),
                    cost_usd: total_cost,
                },
            );
            return Ok(());
        }
    }

    let mut at = 0;
    let mut seq = -1i64;
    let mut runs = vec![0u32; steps.len()];
    'steps: while setup_ok && at < steps.len() {
        seq += 1;
        runs[at] += 1;
        let step = &steps[at];
        let action = &step.action;
        let mut failed = false;
        let mut outcome = String::new();
        'step: {
            match action.kind {
                Kind::Operation => {
                    let env = step_env(StepEnv {
                        job_id,
                        step_name: &action.name,
                        effect_log: &effect_log,
                        input_dir: &idir,
                        input_fields,
                        output_paths: &output_paths,
                        project,
                        repo,
                        home: &f.paths.home,
                        workflow_env,
                        secrets: &secrets,
                        dry_run,
                    });
                    let ran = operation_step::run(operation_step::OperationStep {
                        f,
                        job_id,
                        seq,
                        step,
                        trust,
                        env,
                        repo_checks: &repo_checks,
                        scratch: &scratch,
                        idir: &idir,
                        effect_log: &effect_log,
                        timeout,
                        dry_run,
                    })
                    .await?;
                    total_cost += ran.charged;
                    verdict.extend(ran.verdict);
                    if ran.charged > 0.0
                        && let Some(l) = limits
                        && total_cost > l.budget_usd
                    {
                        needs_human = true;
                        ok = false;
                        verdict.push(over_budget(&action.name, total_cost, l.budget_usd));
                        break 'steps;
                    }
                    if !ran.ok {
                        failed = true;
                        break 'step;
                    }
                    // `produces = ["interface"]` (`ActionDef::yields_interface`)
                    // is the same vocabulary a build workflow's operation uses
                    // to hand its stdout to the next code step
                    // (`operation::run_operation`, `t.interface`); a job step
                    // reads it the same way, via `step_outputs`, so a directive
                    // step after this one sees what an earlier operation
                    // printed — a catalog dump, say — as "the output of step
                    // ...".
                    if action.yields_interface() {
                        step_outputs.push((action.name.clone(), ran.stdout.trim().to_string()));
                    }
                }
                Kind::Directive => {
                    let started_at = unix_now();
                    let ran = match recorded.get(&action.name) {
                        Some(output) => recorded_directive(action, output, &idir, seq),
                        None => {
                            run_directive(RunDirective {
                                f,
                                job_id,
                                seq,
                                project_roles,
                                step,
                                scratch: &scratch,
                                idir: &idir,
                                input_text,
                                step_outputs: &step_outputs,
                                input_bytes,
                            })
                            .await
                        }
                    };
                    let d = match ran {
                        Ok(d) => d,
                        Err(e) => {
                            failed = true;
                            verdict.push(checks::CheckResult {
                                level: "OP".to_string(),
                                name: action.name.clone(),
                                ok: false,
                                tail: format!("{e:#}"),
                                ..Default::default()
                            });
                            break 'step;
                        }
                    };
                    f.store.append_job_step(&JobStep {
                        run: 0,
                        id: 0,
                        job_id,
                        seq,
                        action: action.name.clone(),
                        kind: "directive".to_string(),
                        provider: d.provider,
                        model: d.model,
                        cost_usd: Some(d.cost_usd),
                        started_at,
                        finished_at: Some(unix_now()),
                        exit_code: None,
                        tail: String::new(),
                        outcome: d.outcome.clone(),
                        probabilities: d.probabilities,
                        node: step.node.clone(),
                        output_ref: d
                            .output_ref
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                    })?;
                    outcome = d.outcome;
                    total_cost += d.cost_usd;
                    let step_ok = d.check.ok;
                    verdict.push(d.check);
                    if !step_ok {
                        failed = true;
                        break 'step;
                    }
                    if let Some(l) = limits
                        && total_cost > l.budget_usd
                    {
                        needs_human = true;
                        ok = false;
                        verdict.push(over_budget(&action.name, total_cost, l.budget_usd));
                        break 'steps;
                    }
                    if let Some(p) = &d.output_ref {
                        output_paths.push((action.name.clone(), p.display().to_string()));
                    }
                    step_outputs.push((action.name.clone(), d.output_text));
                }
            }
        }
        match flow::advance(steps, at, (failed, &outcome), &runs, &mut ok, &mut verdict) {
            Some(n) => at = n,
            None => break,
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
    if !needs_human && let Some(l) = limits {
        let within = total_cost <= l.budget_usd;
        ok = ok && within;
        verdict.push(checks::CheckResult {
            level: "L0".to_string(),
            name: "budget".to_string(),
            ok: within,
            tail: format!("${total_cost:.2} of ${:.2}", l.budget_usd),
            ..Default::default()
        });
    }

    let mut state = if needs_human {
        JobState::NeedsHuman
    } else if ok {
        JobState::Ok
    } else {
        JobState::Failed
    };

    // `[limits] on_failure`, honoured (docs/JOBS.md, "The human rung"): a
    // dry run (a fixture replay, `forge job bench`) never retries or asks,
    // the way a `[skip_if]` skip never does either. The decision is made
    // now, against the job row as it stood when the run started, so its
    // `retry_count` reflects this run's own lineage rather than a race
    // with whatever `finish_job` is about to write.
    let mut on_failure = None;
    if !dry_run
        && matches!(state, JobState::Failed | JobState::NeedsHuman)
        && let Some(l) = limits
        && let Some(job_row) = f.store.job(job_id)?
    {
        let input_json: serde_json::Value =
            serde_json::from_str(input_text).unwrap_or(serde_json::Value::Null);
        let contact = trigger_contact(&job_row, trigger, &input_json);
        let action = decide_on_failure(&l.on_failure, job_row.retry_count, contact.as_deref());
        // NeedsHuman is the job analogue of a blocked task: the row that
        // asked a person about it (docs/JOBS.md step 5's `store::jobs`
        // comment on `JobState::NeedsHuman`). A budget overrun already
        // lands there on its own; an assertion or step failure that
        // resolves to `ask:*` now joins it.
        if matches!(action, FailureAction::Ask(_)) {
            state = JobState::NeedsHuman;
        }
        on_failure = Some((action, job_row));
    }

    f.store.finish_job(
        job_id,
        unix_now(),
        state,
        Some(total_cost),
        &serde_json::to_string(&verdict)?,
    )?;
    f.report.emit(
        0,
        Event::JobFinished {
            project,
            workflow,
            job_id,
            state: state.as_str(),
            cost_usd: total_cost,
        },
    );

    if let Some((action, job_row)) = on_failure {
        flow::apply_on_failure(
            f,
            &job_row,
            action,
            &verdict,
            (project, workflow, repo),
            input_text,
        )
        .await;
    }
    Ok(())
}

/// Run a job the worker has already claimed (its store row moved from
/// `queued` to `running` by `Store::claim_next_job`): resolve its pinned
/// workflow, its input written by `start` when it was queued, and its
/// project's repository, then replay the same steps-and-assert executor
/// `--now` runs inline. The workflow is re-resolved by name rather than
/// pinned by `workflow_hash`, same as `--now`'s own steps were already
/// resolved before `create_job`.
async fn run_claimed(f: &Forge, job_id: i64) -> Result<()> {
    let job = f
        .store
        .job(job_id)?
        .with_context(|| format!("job {job_id} vanished before the worker could run it"))?;
    let input_text = std::fs::read_to_string(input_dir(f, job_id).join("input.json"))
        .with_context(|| format!("reading job {job_id} saved input.json"))?;
    let repo = f
        .store
        .first_repo(&job.project)?
        .with_context(|| format!("project {} has no registered repository", job.project))?;
    let repo_path = PathBuf::from(&repo);
    let cfg = config::load_working(&repo_path).await?;
    let (wf, steps, _source) = workflows::resolve_job_for_project(
        &f.paths.home,
        &repo_path,
        &job.landed_sha,
        &job.workflow,
    )?;

    let input_json: serde_json::Value =
        serde_json::from_str(&input_text).context("parsing the job's saved input as JSON")?;
    let input_fields = string_fields(&input_json)?;
    let project_roles = f
        .store
        .project(&job.project)?
        .map(|p| p.role_providers)
        .unwrap_or_default();

    run_now(RunNow {
        f,
        job_id,
        project: &job.project,
        workflow: &job.workflow,
        repo: &repo_path,
        landed_sha: &job.landed_sha,
        steps: &steps,
        assert: &wf.assert,
        skip_if: &wf.skip_if,
        limits: wf.limits.as_ref(),
        trigger: wf.trigger.as_ref(),
        workflow_env: &wf.env,
        dry_run: job.dry_run,
        input_text: &input_text,
        input_fields: &input_fields,
        check_timeout_secs: cfg.check_timeout_secs,
        project_roles: &project_roles,
        recorded: &BTreeMap::new(),
    })
    .await
}

/// The verdict `drive` records for a job that failed before it could run
/// any step, e.g. its workflow no longer resolves or its project lost its
/// repository between being queued and being claimed.
fn executor_error_verdict(e: &anyhow::Error) -> String {
    let verdict = vec![checks::CheckResult {
        level: "OP".to_string(),
        name: "executor".to_string(),
        ok: false,
        tail: format!("{e:#}"),
        ..Default::default()
    }];
    serde_json::to_string(&verdict).unwrap_or_else(|_| "[]".to_string())
}

/// Reconcile interrupted work without repeating any recorded external
/// effect. An operation appends an effect to `effects.log` as it performs
/// it, before the step it belongs to can finish and copy the new lines into
/// `job_effects` (see `run_now`'s `Kind::Operation` arm); a worker that
/// dies, or is aborted, between those two can leave a line in the log the
/// store never learned about. Any such line is still proof the effect
/// happened, so it is recorded here — marked recovered — and treated the
/// same as an effect the store already knew of: the run is never requeued.
pub(crate) use recovery::recover_interrupted;

/// What the worker's claim loop calls on a job it just claimed
/// (`Store::claim_next_job`): run it to completion and report its final
/// state. A job's own failure — a bad archive, a step that errors, a
/// failing assertion — is recorded on the job and never propagated as an
/// error, so it can never stop the worker or requeue an unrelated task
/// (docs/JOBS.md step 1d: "a job's failure never affects a task").
pub async fn drive(f: Arc<Forge>, job_id: i64) -> JobState {
    if let Err(e) = run_claimed(&f, job_id).await {
        let _ = f.store.finish_job(
            job_id,
            unix_now(),
            JobState::Failed,
            Some(0.0),
            &executor_error_verdict(&e),
        );
        // `run_now` never ran (the workflow, config or archive failed to
        // resolve first), so this is the only `JobFinished` this job gets.
        if let Ok(Some(job)) = f.store.job(job_id) {
            f.report.emit(
                0,
                Event::JobFinished {
                    project: &job.project,
                    workflow: &job.workflow,
                    job_id,
                    state: JobState::Failed.as_str(),
                    cost_usd: job.cost_usd.unwrap_or(0.0),
                },
            );
        }
    }
    f.store
        .job(job_id)
        .ok()
        .flatten()
        .map(|j| j.state)
        .unwrap_or(JobState::Failed)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_keeps_short_text_unchanged() {
        assert_eq!(bounded("hello", 10), "hello");
        assert_eq!(bounded("hello", 5), "hello");
    }

    #[test]
    fn bounded_keeps_the_whole_prefix_and_notes_the_cut() {
        assert_eq!(
            bounded("hello world", 5),
            "hello\n... [inputs cut to 5 bytes]"
        );
    }

    #[test]
    fn bounded_backs_off_to_a_char_boundary() {
        // '€' is three bytes; a cut at byte 2 would land inside it.
        assert_eq!(bounded("a€b", 2), "a\n... [inputs cut to 2 bytes]");
    }

    #[test]
    fn string_fields_takes_only_top_level_strings_in_order() {
        let input = serde_json::json!({
            "b": "two",
            "a": "one",
            "n": 5,
            "obj": {"x": "y"},
            "c": "three",
            "flag": true,
        });
        let fields = string_fields(&input).unwrap();
        assert_eq!(
            fields,
            vec![
                ("a".to_string(), "one".to_string()),
                ("b".to_string(), "two".to_string()),
                ("c".to_string(), "three".to_string()),
            ]
        );
    }

    #[test]
    fn string_fields_rejects_a_non_object_input() {
        assert!(string_fields(&serde_json::json!(["a", "b"])).is_err());
        assert!(string_fields(&serde_json::json!("just a string")).is_err());
        assert!(string_fields(&serde_json::json!(5)).is_err());
    }

    fn bin_dir() -> String {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.display().to_string()))
            .unwrap_or_default()
    }

    #[test]
    fn step_env_for_a_real_run_orders_inputs_outputs_env_then_secrets_last() {
        let mut secrets = std::collections::BTreeMap::new();
        secrets.insert("Z_SECRET".to_string(), "zzz".to_string());
        secrets.insert("A_SECRET".to_string(), "aaa".to_string());
        let mut workflow_env = BTreeMap::new();
        workflow_env.insert("MAX_LINES".to_string(), "400".to_string());
        let env = step_env(StepEnv {
            job_id: 7,
            step_name: "mystep",
            effect_log: Path::new("/scratch/effects.log"),
            input_dir: Path::new("/scratch/input"),
            input_fields: &[("Name".to_string(), "bob".to_string())],
            output_paths: &[(
                "my-action".to_string(),
                "/scratch/output-my-action.json".to_string(),
            )],
            project: "acme",
            repo: Path::new("/repo/acme"),
            home: Path::new("/home/forge"),
            workflow_env: &workflow_env,
            secrets: &secrets,
            dry_run: false,
        });
        assert_eq!(
            env,
            vec![
                ("FORGE_JOB_ID".to_string(), "7".to_string()),
                ("FORGE_STEP".to_string(), "mystep".to_string()),
                (
                    "FORGE_EFFECT_LOG".to_string(),
                    "/scratch/effects.log".to_string()
                ),
                ("FORGE_INPUT_DIR".to_string(), "/scratch/input".to_string()),
                ("FORGE_PROJECT".to_string(), "acme".to_string()),
                ("FORGE_REPO_DIR".to_string(), "/repo/acme".to_string()),
                ("FORGE_HOME".to_string(), "/home/forge".to_string()),
                ("FORGE_BIN_DIR".to_string(), bin_dir()),
                ("FORGE_INPUT_NAME".to_string(), "bob".to_string()),
                (
                    "FORGE_OUTPUT_MY_ACTION".to_string(),
                    "/scratch/output-my-action.json".to_string()
                ),
                ("MAX_LINES".to_string(), "400".to_string()),
                ("A_SECRET".to_string(), "aaa".to_string()),
                ("Z_SECRET".to_string(), "zzz".to_string()),
            ]
        );
    }

    #[test]
    fn step_env_for_a_dry_run_adds_the_flag_before_inputs_and_never_a_real_run() {
        let secrets = std::collections::BTreeMap::new();
        let workflow_env = BTreeMap::new();
        let dry = step_env(StepEnv {
            job_id: 1,
            step_name: "s",
            effect_log: Path::new("/log"),
            input_dir: Path::new("/in"),
            input_fields: &[],
            output_paths: &[],
            project: "acme",
            repo: Path::new("/repo/acme"),
            home: Path::new("/home/forge"),
            workflow_env: &workflow_env,
            secrets: &secrets,
            dry_run: true,
        });
        assert_eq!(
            dry,
            vec![
                ("FORGE_JOB_ID".to_string(), "1".to_string()),
                ("FORGE_STEP".to_string(), "s".to_string()),
                ("FORGE_EFFECT_LOG".to_string(), "/log".to_string()),
                ("FORGE_INPUT_DIR".to_string(), "/in".to_string()),
                ("FORGE_PROJECT".to_string(), "acme".to_string()),
                ("FORGE_REPO_DIR".to_string(), "/repo/acme".to_string()),
                ("FORGE_HOME".to_string(), "/home/forge".to_string()),
                ("FORGE_BIN_DIR".to_string(), bin_dir()),
                ("FORGE_DRY_RUN".to_string(), "1".to_string()),
            ]
        );

        let real = step_env(StepEnv {
            job_id: 1,
            step_name: "s",
            effect_log: Path::new("/log"),
            input_dir: Path::new("/in"),
            input_fields: &[],
            output_paths: &[],
            project: "acme",
            repo: Path::new("/repo/acme"),
            home: Path::new("/home/forge"),
            workflow_env: &workflow_env,
            secrets: &secrets,
            dry_run: false,
        });
        assert!(!real.iter().any(|(k, _)| k == "FORGE_DRY_RUN"));
    }

    #[test]
    fn executor_error_verdict_names_the_executor_check_and_quotes_the_error() {
        let err = anyhow::anyhow!("workflow no longer resolves");
        let verdict = executor_error_verdict(&err);
        let parsed: Vec<checks::CheckResult> = serde_json::from_str(&verdict).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].level, "OP");
        assert_eq!(parsed[0].name, "executor");
        assert!(!parsed[0].ok);
        assert_eq!(parsed[0].tail, "workflow no longer resolves");
    }
}
