//! The running system: claim, run, repeat, stay up. `drive` is the one
//! place a fault becomes an outcome: a task fault fails the task; an
//! environment fault puts the task back in the queue and stops the worker.
//!
//! `forge work` runs up to `--jobs` tasks at once, polls the queue every
//! `--poll` seconds when it is empty, and exits when told to. The first
//! SIGINT/SIGTERM stops claiming and lets running attempts finish; a
//! second one aborts them (the sandbox tree dies with the child) and puts
//! their tasks back in the queue.

use crate::ctx::{Forge, Paths};
use crate::engine::{self, Fault};
use crate::job;
use crate::report::Event;
use crate::store::{Direction, JobState, Message, Task, TaskState};
use crate::unix_now;
use crate::workflows;
use crate::{config, git};
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;

pub(crate) mod capacity;
mod claim;
mod holds;
mod schedule;
pub(crate) use holds::held_initiatives;
use holds::new_holds;
use schedule::{RefusalLog, schedule_tick};

/// What one slot of the worker's `--jobs` cap finished running: a task
/// (`src/engine.rs`) or a job (`src/job.rs`) — the worker claims both from
/// the same queue of free slots (docs/JOBS.md step 1d).
enum WorkResult {
    Task(i64, Result<TaskState>),
    Job(i64, JobState),
}

/// Whether a process with this pid exists, by signal 0: no signal is sent,
/// only the permission and existence checks run. `ESRCH` means no such
/// process; `EPERM` means one exists that is not ours to signal. Works on
/// every unix, `/proc` or not (macOS has none).
pub fn pid_alive(pid: i64) -> bool {
    // 0 and negative pids name process groups, not a process.
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Whether `pid` is alive and still the process that recorded `start`
/// (`crate::store::start_of`): the check orphan recovery runs on a claimed
/// row's owner (`Caller::is_orphan`), applied to a `workers` row so a pid
/// the table calls live after a reuse is not mistaken for the worker that
/// registered it (REVIEW-4 E2-2). An empty `start` (a row from before it
/// was recorded) or a `start_of` that cannot read the process proves
/// nothing, so it counts as still alive.
pub fn worker_alive(pid: i64, start: &str) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    match crate::store::start_of(pid) {
        Some(current) if !start.is_empty() => current == start,
        _ => true,
    }
}

/// The worker as `worker.pid` says: pid, its binary, whether it is alive,
/// and whether that binary was rebuilt underneath it since it started.
pub struct WorkerStatus {
    pub pid: i64,
    pub exe: String,
    pub running: bool,
    /// `None` when there is no `/proc` to read the running binary from
    /// (macOS): the stale-binary check is skipped, not passed.
    pub stale: Option<bool>,
}

pub fn worker_status(paths: &Paths) -> Option<WorkerStatus> {
    let text = std::fs::read_to_string(paths.home.join("worker.pid")).ok()?;
    let mut it = text.lines().next().unwrap_or_default().split_whitespace();
    let pid: i64 = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let exe = it.next().unwrap_or("").to_string();
    let running = pid > 0 && pid_alive(pid);
    let stale = if !running {
        Some(false)
    } else if Path::new("/proc").exists() {
        Some(
            std::fs::read_link(format!("/proc/{pid}/exe"))
                .map(|p| p.to_string_lossy().ends_with(" (deleted)"))
                .unwrap_or(false),
        )
    } else {
        None
    };
    Some(WorkerStatus {
        pid,
        exe,
        running,
        stale,
    })
}

/// Run one claimed task to its end. `Err` means the worker environment is
/// broken; the task has already been requeued.
pub async fn drive(f: Arc<Forge>, id: i64) -> Result<TaskState> {
    drive_with_wait(f, id, false).await
}

/// Direct runs can retain their foreground slot while provider arms are held.
pub async fn drive_with_wait(f: Arc<Forge>, id: i64, wait: bool) -> Result<TaskState> {
    match engine::run_task(f.clone(), id, wait).await {
        Ok(TaskState::Blocked) => {
            let ended = f.store.task(id)?.context("ended task missing")?;
            // A question addressed to someone other than the operator
            // (an intake interview's contact, say) is not the
            // supervisor's to rule on: leave it for the channel plugin,
            // and never spend a supervisor attempt on it.
            let addressed_elsewhere = f
                .store
                .task(id)?
                .and_then(|t| crate::supervisor::addressed_elsewhere(&t));
            let refused = f
                .store
                .task(id)?
                .is_some_and(|t| t.reason.starts_with(crate::ctx::EGRESS_REFUSAL));
            if refused {
                // No ruling changes the backend; the operator's config does.
            } else if let Some(note) = addressed_elsewhere {
                f.report.emit(id, Event::Note { text: &note });
            } else if let Some(n) = crate::supervisor::demotion_as_task(&f, id)
                .await
                .unwrap_or_else(|e| {
                    f.report.emit(
                        id,
                        Event::Note {
                            text: &format!("demotion rule error: {e:#}"),
                        },
                    );
                    None
                })
            {
                f.report.emit(
                    id,
                    Event::Note {
                        text: &format!("the demotion names a reproducible defect; filed task {n}"),
                    },
                );
            } else if let Err(e) = crate::supervisor::supervise(&f, id).await {
                // The rung before the human: the supervisor reads the
                // record and answers, files a prerequisite, or
                // escalates. Its own failure is a note, never a task
                // failure.
                f.report.emit(
                    id,
                    Event::Note {
                        text: &format!("supervisor error: {e:#}"),
                    },
                );
            }
            crate::audience::emit_recovered(&f, &ended).await?;
            Ok(TaskState::Blocked)
        }
        Ok(TaskState::Failed) => {
            let ended = f.store.task(id)?.context("ended task missing")?;
            on_failed(&f, id).await;
            crate::audience::emit_recovered(&f, &ended).await?;
            Ok(TaskState::Failed)
        }
        Ok(state) => Ok(state),
        Err(Fault::Task(e)) => {
            f.report.emit(
                id,
                Event::Note {
                    text: &format!("ERROR    {e:#}"),
                },
            );
            if let Some(mut t) = f.store.task(id)? {
                t.state = TaskState::Failed;
                t.reason = format!("error: {e:#}");
                t.finished_at = Some(unix_now());
                t.worker_pid = None;
                f.store.update_task(&t)?;
                engine::finish_fault(&f, &t)?;
            }
            let ended = f.store.task(id)?.context("ended task missing")?;
            on_failed(&f, id).await;
            crate::audience::emit_recovered(&f, &ended).await?;
            Ok(TaskState::Failed)
        }
        Err(Fault::Env(e)) if e.downcast_ref::<engine::TaskEnv>().is_some() => {
            // The count of consecutive claims rides in the reason the last
            // requeue left: any other outcome rewrites it.
            let prior = f
                .store
                .task(id)?
                .and_then(|t| env_claims(&t.reason))
                .unwrap_or(0);
            let claims = prior + 1;
            let why = format!("{ENV_REQUEUE_PREFIX} (claim {claims} of {ENV_CLAIM_LIMIT}): {e:#}");
            f.store
                .requeue(id, &crate::store::Owner::this_process(), &why)?;
            let mut state = TaskState::Queued;
            if claims >= ENV_CLAIM_LIMIT
                && let Some(mut t) = f.store.task(id)?
                && t.state == TaskState::Queued
            {
                t.state = TaskState::Blocked;
                t.reason = format!(
                    "{e:#} (environment error on {claims} consecutive claims; fix it, then forge retry {id})"
                );
                t.finished_at = Some(unix_now());
                t.worker_pid = None;
                f.store.update_task(&t)?;
                f.report.emit(id, Event::Note { text: &t.reason });
                state = TaskState::Blocked;
            }
            Ok(state)
        }
        Err(Fault::Env(e)) => {
            f.store.requeue(
                id,
                &crate::store::Owner::this_process(),
                &format!("worker environment error: {e:#}"),
            )?;
            Err(e.context(format!(
                "worker cannot run task {id}; it is back in the queue"
            )))
        }
    }
}

/// Consecutive environment-error claims a task may fail before it blocks.
const ENV_CLAIM_LIMIT: u32 = 3;
const ENV_REQUEUE_PREFIX: &str = "worker environment error";

/// The claim count recorded in a reason `drive` wrote for a task-scoped
/// environment error requeue, `None` for any other reason.
fn env_claims(reason: &str) -> Option<u32> {
    let rest = reason
        .strip_prefix("requeued: ")?
        .strip_prefix(ENV_REQUEUE_PREFIX)?
        .strip_prefix(" (claim ")?;
    rest.split(' ').next()?.parse().ok()
}

/// The kernel's follow-up rule for a task that just ended `Failed`
/// (`src/supervisor/mechanic.rs`): a retry, a guided refile, or an
/// operator decision. Its own failure is a note, never the task's.
async fn on_failed(f: &Forge, id: i64) {
    if let Err(e) = crate::supervisor::mechanic::act(f, id).await {
        f.report.emit(
            id,
            Event::Note {
                text: &format!("mechanic error: {e:#}"),
            },
        );
    }
}

/// The rolling 24-hour cap, when one is set. `Some(message)` when nothing
/// more may start.
pub fn day_budget_reached(f: &Forge) -> Result<Option<String>> {
    let Some(cap) = f.budget.per_day_usd else {
        return Ok(None);
    };
    let spent = f.store.spent_since(unix_now() - 86_400)?;
    Ok((spent >= cap).then(|| {
        format!(
            "daily budget reached: ${spent:.2} of ${cap:.2} in the last 24h (per_day_usd in {})",
            p_config(f)
        )
    }))
}

/// `provider` held: its login refused (see `login_hold`), else its own window
/// at or over its own cap by its latest sample (never another provider's), a
/// passed reset holding nothing. The message and the second to look again.
pub fn window_hold(f: &Forge, provider: &str) -> Result<Option<(String, i64)>> {
    if let h @ Some(_) = crate::login_hold::held(f, provider)? {
        return Ok(h);
    }
    let Some(s) = f.store.latest_rate_limit(provider)? else {
        return Ok(None);
    };
    let caps = f
        .providers
        .get(provider)
        .map(|p| (p.five_hour_max, p.seven_day_max))
        .unwrap_or((f.budget.five_hour_max, f.budget.seven_day_max));
    Ok(hold_from_sample(&s, caps, unix_now()))
}

/// `window_hold`'s decision on one sample: the tightest window at or
/// over its cap whose reset is still ahead. A sample older than the
/// window it describes is stale, whatever reset it names: a five-hour
/// window seen more than five hours ago has reset since, so it holds
/// nothing (a mis-read reset time once held a provider for a day).
fn hold_from_sample(
    s: &crate::store::RateLimitSample,
    (five_hour_max, seven_day_max): (f64, f64),
    now: i64,
) -> Option<(String, i64)> {
    let mut hold: Option<(String, i64)> = None;
    for (name, util, resets, cap, span) in [
        (
            "5h",
            s.five_hour,
            s.five_hour_resets,
            five_hour_max,
            5 * 3600,
        ),
        (
            "7d",
            s.seven_day,
            s.seven_day_resets,
            seven_day_max,
            7 * 86_400,
        ),
    ] {
        let (Some(u), Some(r)) = (util, resets) else {
            continue;
        };
        if now - s.seen_at > span {
            continue;
        }
        if u >= cap && r > now && hold.as_ref().is_none_or(|(_, until)| r > *until) {
            hold = Some((
                format!(
                    "rate window {name} at {:.0}% (cap {:.0}%), resets in {}m",
                    u * 100.0,
                    cap * 100.0,
                    (r - now + 59) / 60
                ),
                r,
            ));
        }
    }
    hold
}

/// The role the task's *next agent step* will actually run under: the
/// contract of the first directive step in its resolved workflow that the
/// record does not already show done (`engine::resume_done`, the same
/// resume rule `run_task` builds its cursor from), so a resumed task whose
/// code step already succeeded is judged by its review, not its code.
/// Falls back to "code" on any failure to resolve (unknown/broken
/// workflow, no undone directive step) or to read the record: a provider
/// that fails to resolve is never held here, the real error surfaces when
/// the task actually runs.
fn first_role(f: &Forge, t: &Task) -> String {
    let resolved: workflows::Resolved = if !t.actions_json.is_empty() {
        match serde_json::from_str(&t.actions_json) {
            Ok(r) => r,
            Err(_) => return "code".into(),
        }
    } else {
        match workflows::resolve(&f.paths.home, &t.workflow) {
            Ok(r) => r,
            Err(_) => return "code".into(),
        }
    };
    let done = f
        .store
        .attempts(t.id)
        .map(|prior| engine::resume_done(&prior))
        .unwrap_or_default();
    resolved
        .steps
        .into_iter()
        .enumerate()
        .find(|(i, s)| {
            s.action.kind == workflows::Kind::Directive && !done.contains(&(*i as i64 + 1))
        })
        .map(|(_, s)| s.action.contract.as_str().to_string())
        .unwrap_or_else(|| "code".into())
}

/// How the claim loop treats `t` now: run under the provider its next
/// agent step resolves to (see `first_role`), or, when that provider is
/// held and `t`'s arm for the role was drawn from `experiment.toml`, under
/// an arm of the same role that is not (`redraw::decide`). `None` when the
/// provider does not resolve: the real error surfaces when the task runs.
/// Nothing is written here.
fn route_candidate(f: &Forge, t: &Task) -> Option<crate::redraw::Routing> {
    let role = first_role(f, t);
    let provider = f.effective_provider(t, &role).ok()?.name.clone();
    let hold = |p: &str| window_hold(f, p).ok().flatten();
    // The experiment is read only once the drawn provider is known to be
    // held, so a free queue never touches `experiment.toml`.
    let weights = hold(&provider)
        .and_then(|_| crate::redraw::arms(&f.paths.home, &role, |p| f.providers.contains_key(p)));
    Some(crate::redraw::decide(
        t,
        &role,
        &provider,
        weights.as_ref(),
        hold,
    ))
}

/// Whether the claim loop must skip `t` for its provider: held, and no
/// other arm of its role could take it. A held drawn arm that another
/// provider can run is re-drawn instead (recorded on the task's
/// `explore`, noted on its event stream, and announced), and the task is
/// not skipped.
fn provider_is_held(f: &Forge, t: &Task) -> bool {
    match route_candidate(f, t) {
        None | Some(crate::redraw::Routing::Free) => false,
        Some(crate::redraw::Routing::Held { .. }) => true,
        Some(crate::redraw::Routing::Redrawn { explore, note }) => {
            match f.store.set_explore(t.id, &explore) {
                Ok(true) => {
                    eprintln!("task {}: {note}", t.id);
                    f.report.emit(t.id, Event::Note { text: &note });
                    false
                }
                // Claimed or edited in between: leave it to the next pass.
                _ => true,
            }
        }
    }
}

/// `msg` led by the provider it is about, unless it already is (a login
/// hold's message names its provider).
fn named(provider: &str, msg: &str) -> String {
    if msg.starts_with(&format!("{provider}:")) {
        msg.to_string()
    } else {
        format!("{provider}: {msg}")
    }
}

/// Print the idle line when it differs from the one last printed.
fn announce_idle(f: &Forge, held: &[i64], last: &mut Option<String>) {
    let reason = f
        .store
        .queue_breakdown(held)
        .ok()
        .and_then(|q| idle_reason(&q));
    if reason != *last {
        if let Some(r) = &reason {
            eprintln!("{r}");
        }
        *last = reason;
    }
}

/// One queued task and why it is or is not claimable.
type QueueEntry = (i64, crate::store::QueueStatus);

/// The idle line for a queue where nothing can be claimed, or `None` when
/// the queue is empty or some task is claimable (so anything still idle is
/// a provider hold, announced on its own).
fn idle_reason(queue: &[QueueEntry]) -> Option<String> {
    use crate::store::QueueStatus::*;
    if queue.is_empty() || queue.iter().any(|(_, s)| *s == Claimable) {
        return None;
    }
    let (mut blocked, mut active, mut held) = (0, 0, 0);
    let mut blockers: Vec<i64> = Vec::new();
    for (_, s) in queue {
        match s {
            WaitsOnBlocked { dep } => {
                blocked += 1;
                if !blockers.contains(dep) {
                    blockers.push(*dep);
                }
            }
            WaitsOnActive { .. } => active += 1,
            HeldInitiative { .. } => held += 1,
            Claimable => {}
        }
    }
    let mut parts = Vec::new();
    if blocked > 0 {
        let ids: Vec<String> = blockers.iter().map(|d| d.to_string()).collect();
        let noun = if ids.len() == 1 { "task" } else { "tasks" };
        parts.push(format!(
            "{blocked} wait on blocked {noun} {}",
            ids.join(", ")
        ));
    }
    if active > 0 {
        parts.push(format!("{active} on active work"));
    }
    if held > 0 {
        parts.push(format!("{held} in held initiatives"));
    }
    Some(format!(
        "idle: {} queued, none claimable: {}",
        queue.len(),
        parts.join(", ")
    ))
}

/// The tightest (soonest-resetting) hold among every queued, unblocked
/// task's own provider (the one `first_role` says its next agent step
/// will run under, or an arm it would be re-drawn to), when *none* of
/// them can be claimed right now; `None` as soon as one candidate can run
/// under a provider that is not held, since the caller can claim it
/// instead of waiting. The message names the provider and how many
/// tasks wait on it: those outside `held_initiatives`, whose own hold is
/// announced on its own (`new_holds`).
fn tightest_provider_hold(f: &Forge, held_initiatives: &[i64]) -> Result<Option<(String, i64)>> {
    let mut tightest: Option<(String, i64)> = None;
    let queued = f.store.queued_unblocked(held_initiatives)?;
    let n = queued.len();
    for t in queued {
        match route_candidate(f, &t) {
            None | Some(crate::redraw::Routing::Free) => return Ok(None),
            Some(crate::redraw::Routing::Redrawn { .. }) => return Ok(None),
            Some(crate::redraw::Routing::Held {
                provider,
                msg,
                until,
            }) => {
                if tightest.as_ref().is_none_or(|(_, u)| until < *u) {
                    tightest = Some((named(&provider, &msg), until));
                }
            }
        }
    }
    Ok(tightest.map(|(msg, until)| (format!("{msg}; holding, {n} task(s) queued"), until)))
}

fn p_config(f: &Forge) -> String {
    f.paths.home.join("config.toml").display().to_string()
}

/// The name of the first directive step `t`'s resolved workflow runs,
/// the same approximation `first_role` makes for the provider it holds:
/// good enough to tell an `intake` task (whose only directive is
/// `interview`) apart from every other workflow.
fn first_directive_name(f: &Forge, t: &Task) -> Option<String> {
    let resolved: workflows::Resolved = if !t.actions_json.is_empty() {
        serde_json::from_str(&t.actions_json).ok()?
    } else {
        workflows::resolve(&f.paths.home, &t.workflow).ok()?
    };
    resolved
        .steps
        .into_iter()
        .find(|s| s.action.kind == workflows::Kind::Directive)
        .map(|s| s.action.name)
}

/// The operator's `[intake] max_questions_per_day` cap, when `t`'s next
/// agent step is the `interview` directive: at the cap, the worker
/// leaves it queued rather than start a turn that would ask another
/// question today (see docs/INTAKE.md).
fn intake_is_held(f: &Forge, t: &Task) -> bool {
    if first_directive_name(f, t).as_deref() != Some("interview") {
        return false;
    }
    f.store
        .interview_questions_since(unix_now() - 86_400)
        .map(|n| n >= f.intake.max_questions_per_day as i64)
        .unwrap_or(false)
}

/// Every run workflow that resolves for `project` right now (the schedule
/// tick keeps the ones with a schedule trigger, `message_triggers` the ones
/// with a message trigger; `what` prefixes what is noted on stderr): its
/// own repository's `.forge/workflows/*.toml` at its latest landed commit,
/// and the operator's catalog, resolved by name the same way `forge job start` resolves any other workflow — the
/// repository first, the catalog only for a name the repository does not
/// have there (docs/JOBS.md, "Where an automation lives"). A project with
/// no registered repository, or whose repository's `base_branch` has
/// never landed anything, contributes nothing. A broken workflow file
/// (the catalog's own, or one under this project's `.forge/workflows/`)
/// is noted on stderr and skipped rather than stopping the tick for every
/// other project.
pub(crate) async fn project_run_workflows(
    f: &Forge,
    project: &str,
    what: &str,
) -> Vec<(String, workflows::Workflow, workflows::JobSource, String)> {
    let mut out = Vec::new();
    let repo = match f.store.first_repo(project) {
        Ok(Some(r)) => r,
        Ok(None) => return out,
        Err(e) => {
            eprintln!("{what}: {project}: {e:#}");
            return out;
        }
    };
    let repo_path = Path::new(&repo);
    let cfg = match config::load_working(repo_path).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{what}: {project}: {e:#}");
            return out;
        }
    };
    let landed_sha =
        match git::rev_parse(repo_path, &format!("refs/heads/{}", cfg.base_branch)).await {
            Ok(s) => s,
            Err(_) => return out, // never landed anything yet: nothing to run a schedule against
        };
    let mut names: Vec<String> = match workflows::load_all_at(repo_path, &landed_sha) {
        Ok(v) => v
            .into_iter()
            .filter(|w| w.kind == workflows::WorkflowKind::Run)
            .map(|w| w.name)
            .collect(),
        Err(e) => {
            eprintln!("{what}: {project}: {e:#}");
            Vec::new()
        }
    };
    match workflows::load_catalog(&f.paths.home) {
        Ok(cat) => {
            for (name, w) in cat.workflows {
                if w.kind == workflows::WorkflowKind::Run && !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        Err(e) => eprintln!("{what}: {e:#}"),
    }
    for name in names {
        match workflows::resolve_job_for_project(&f.paths.home, repo_path, &landed_sha, &name) {
            Ok((wf, _steps, source)) => out.push((name, wf, source, landed_sha.clone())),
            Err(e) => eprintln!("{what}: {project}/{name}: {e:#}"),
        }
    }
    out
}

/// One run workflow of one project, resolved for this pass of the poll loop
/// (`project_run_workflows`): what the schedule tick and the event tick both
/// read, resolved once between them.
struct TickRun {
    project: String,
    name: String,
    wf: workflows::Workflow,
    source: workflows::JobSource,
    landed_sha: String,
}

/// Every project's run workflows for this pass, in project order.
async fn tick_run_workflows(f: &Forge) -> Result<Vec<TickRun>> {
    let mut runs = Vec::new();
    for project in f.store.list_active_projects()? {
        for (name, wf, source, landed_sha) in
            project_run_workflows(f, &project.name, "worker tick").await
        {
            runs.push(TickRun {
                project: project.name.clone(),
                name,
                wf,
                source,
                landed_sha,
            });
        }
    }
    Ok(runs)
}

/// The most `events.jsonl` one workflow examines in one tick; a worker that
/// was down for a long while catches up over several ticks rather than
/// holding the whole backlog in memory.
const EVENT_TICK_BYTES: u64 = 8 * 1024 * 1024;

/// The project an event belongs to: the one it names (a deploy's, a job's),
/// else its task's. `None` for an event that names neither (a note, say),
/// which belongs to no project's workflows.
fn event_project(f: &Forge, ev: &serde_json::Value) -> Option<String> {
    if let Some(p) = ev["project"].as_str() {
        return Some(p.to_string());
    }
    let task = ev["task"].as_i64().filter(|t| *t > 0)?;
    f.store.task(task).ok().flatten()?.project
}

/// The ticks a live worker runs each pass: run-workflow resolution, then
/// the schedule and event triggers over what it resolved, then the drafts
/// whose last missing action has landed (`workflows::draft::reconcile`). A worker
/// that is superseded or stopping fires none of them: its older code would
/// resolve workflows, queue jobs beside the successor and move the shared
/// event cursor. Each tick is its own step: one that fails is logged and
/// the others still run, so a tick that fails every pass (an event log it
/// cannot read) never keeps the pass from claiming. Whether the ticks ran.
async fn run_ticks(
    f: &Forge,
    refusals: &mut RefusalLog,
    superseded: bool,
    stopping: bool,
) -> Result<bool> {
    if superseded || stopping {
        return Ok(false);
    }
    let runs = tick_run_workflows(f).await.unwrap_or_else(|e| {
        eprintln!("worker tick failed (workflows); continuing: {e:#}");
        Vec::new()
    });
    if let Err(e) = schedule_tick(f, &runs, refusals).await {
        eprintln!("worker tick failed (schedule); continuing: {e:#}");
    }
    if let Err(e) = event_tick(f, &runs).await {
        eprintln!("worker tick failed (event); continuing: {e:#}");
    }
    if let Err(e) = workflows::draft::reconcile(&f.paths.home).await {
        eprintln!("worker tick failed (drafts); continuing: {e:#}");
    }
    Ok(true)
}

/// The worker's event trigger (docs/JOBS.md, "Triggers" and "Build order"
/// step 3): for every project's run workflow with `[trigger] on = "event"`,
/// read the events in `events.jsonl` past the offset this workflow last
/// examined (`store::event_cursor`) and start one job for each event of the
/// declared `type` that belongs to the project, `trigger_ref` its offset
/// (`job::start_event`) and the event's own JSON its input. The cursor then
/// moves past everything read, whatever its type. A workflow the tick sees
/// for the first time starts at the end of the log — a new automation is
/// never backfilled with the whole history — and rotation drains the preceding generation before reading the new file. A job's own
/// `job_started` and `job_finished` events never start the workflow that
/// ran it. Called once per pass of the poll loop, after the schedule tick;
/// cheap when no workflow has an event trigger or nothing was appended.
async fn event_tick(f: &Forge, runs: &[TickRun]) -> Result<()> {
    let path = f.paths.home.join("events.jsonl");
    for run in runs {
        let Some(trigger) = run
            .wf
            .trigger
            .as_ref()
            .filter(|t| t.on == workflows::TriggerOn::Event)
        else {
            continue;
        };
        let (project, name) = (run.project.as_str(), run.name.as_str());
        let end = crate::report::log::snapshot(&path)?.to_string();
        let stored = match f.store.event_cursor(project, name) {
            Ok(Some(c)) => c,
            Ok(None) => {
                if let Err(e) = f.store.set_event_cursor(project, name, &end) {
                    eprintln!("event tick: {project}/{name}: {e:#}");
                }
                continue;
            }
            Err(e) => {
                eprintln!("event tick: {project}/{name}: {e:#}");
                continue;
            }
        };
        let batch = crate::report::log::read(&path, stored.parse()?, EVENT_TICK_BYTES)?;
        let next = batch.next.to_string();
        for (offset, _, line) in batch.lines {
            let Ok(ev) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let Some(kind) = ev["type"].as_str().filter(|k| trigger.matches_event(k)) else {
                continue;
            };
            if event_project(f, &ev).as_deref() != Some(project) {
                continue;
            }
            // A job's own start and finish are events too; the workflow
            // that produced them must not start itself again from them.
            if ev["job_id"].is_number() && ev["workflow"].as_str() == Some(name) {
                continue;
            }
            let at = ev["ts"].as_i64().unwrap_or_else(unix_now);
            match job::start_event(crate::job::StartEvent {
                f,
                project,
                workflow: name,
                landed_sha: &run.landed_sha,
                wf: &run.wf,
                source: run.source,
                offset: &offset.to_string(),
                at,
                input: &line,
            }) {
                Ok(Some(id)) => {
                    eprintln!("======== job {id} starting (event {kind} on {project}, {name})");
                }
                Ok(None) => {}
                Err(e) => eprintln!("event tick: {project}/{name}: {e:#}"),
            }
        }
        if next != stored
            && let Err(e) = f.store.set_event_cursor(project, name, &next)
        {
            eprintln!("event tick: {project}/{name}: {e:#}");
        }
    }
    Ok(())
}

/// The message trigger (docs/JOBS.md, "Triggers" and "Build order" step 3):
/// `forge message record` calls this once an inbound message is recorded.
/// Every run workflow that resolves for the message's project — the same
/// resolution the schedule tick uses — whose `[trigger]` is `on =
/// "message"` with a `contact` of `"*"` or the message's own starts one job
/// (`job::start_message`), queued for the worker and never run inline.
/// Idempotent per message: a workflow that already started a job for this
/// message id starts none. Returns `(workflow, job id)` for each job
/// started; a workflow whose start fails (its `per_day` cap, say) is noted
/// on stderr and skipped, so one refusal never hides the others. An
/// outbound message fires nothing.
pub async fn message_triggers(f: &Forge, m: &Message) -> Vec<(String, i64)> {
    let mut started = Vec::new();
    if m.direction != Direction::In {
        return started;
    }
    for (name, wf, source, landed_sha) in
        project_run_workflows(f, &m.project, "message trigger").await
    {
        if !wf
            .trigger
            .as_ref()
            .is_some_and(|t| t.matches_message(&m.contact))
        {
            continue;
        }
        match job::start_message(f, &m.project, &name, &landed_sha, &wf, source, m) {
            Ok(Some(id)) => started.push((name, id)),
            Ok(None) => {}
            Err(e) => eprintln!("message trigger: {}/{name}: {e:#}", m.project),
        }
    }
    started
}

/// The run workflow a webhook fires (docs/JOBS.md, "Triggers"): among
/// every run workflow that resolves for `project` — the resolution the
/// schedule tick and `message_triggers` use — the one whose `[trigger]` is
/// `on = "webhook"` with this `name`. An error says which of "none" and
/// "more than one" it was, since a hook that fires two automations is
/// refused rather than guessed at.
pub async fn webhook_workflow(
    f: &Forge,
    project: &str,
    name: &str,
) -> Result<(String, workflows::Workflow, workflows::JobSource, String)> {
    let mut matches: Vec<_> = project_run_workflows(f, project, "webhook trigger")
        .await
        .into_iter()
        .filter(|(_, wf, _, _)| wf.trigger.as_ref().is_some_and(|t| t.matches_webhook(name)))
        .collect();
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => bail!("no run workflow in project {project} has a webhook trigger named {name}"),
        _ => bail!(
            "more than one run workflow in project {project} has a webhook trigger named {name}: {}",
            matches
                .iter()
                .map(|(w, ..)| w.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The slots this worker may fill: the machine's `jobs`, less the attempts
/// other live workers still run (a draining predecessor's, or the
/// successor's), so a handoff never takes the box past `jobs` between them.
pub fn slot_budget(jobs: usize, running_elsewhere: usize) -> usize {
    jobs.saturating_sub(running_elsewhere).min(jobs)
}

/// `slot_budget`, reading what the other live workers hold right now.
fn free_slots(f: &Forge, pid: i64, jobs: usize) -> usize {
    slot_budget(
        jobs,
        f.store.running_elsewhere(pid, worker_alive).unwrap_or(0),
    )
}

pub struct WorkOpts {
    pub jobs: Option<usize>,
    /// Seconds between queue polls when idle; `None` exits when idle.
    pub poll: Option<u64>,
    pub max_tasks: Option<u32>,
}

/// The worker's SIGINT and SIGTERM listeners, installed once for its whole
/// life. They used to be created afresh inside every `select!` iteration,
/// and a tokio signal stream only sees signals that arrive after it is
/// created: a signal landing in the gap between one iteration's stream
/// being dropped and the next one's being installed was lost. The gap is
/// microseconds on an idle box and long enough under load that the e2e
/// suite hit it about one run in ten on 2026-09-18 (an idle worker that
/// never stopped on SIGTERM; a second SIGINT that never aborted).
struct Shutdown {
    int: tokio::signal::unix::Signal,
    term: tokio::signal::unix::Signal,
}

impl Shutdown {
    fn install() -> Self {
        use tokio::signal::unix::{SignalKind, signal};
        Shutdown {
            int: signal(SignalKind::interrupt()).expect("SIGINT handler"),
            term: signal(SignalKind::terminate()).expect("SIGTERM handler"),
        }
    }

    /// Resolves on the next SIGINT or SIGTERM. Signals that arrived while
    /// nothing was awaiting this are still delivered, since the streams
    /// outlive every `select!` that polls them.
    async fn recv(&mut self) {
        tokio::select! {
            _ = self.int.recv() => {}
            _ = self.term.recv() => {}
        }
    }
}

/// Tasks and jobs a dead worker left running go back in the queue, at startup
/// (`just_started`: a row under this very pid is a previous incarnation's) and
/// on every claim-loop pass. Writes are guarded on the owner listed. At
/// startup only, every private provider-state copy is offered as a
/// write-back once (docs/REVIEW-4.md #1.9): a launch this worker's dead
/// predecessor aborted mid-flight never reached its own.
fn recover_orphans(f: &Forge, just_started: bool) -> Result<()> {
    if just_started {
        crate::login::write_back_all_private_copies(&f.paths.home, &f.paths.worktrees);
    }
    let caller = crate::store::Caller::this_process(just_started);
    for (id, owner) in f.store.orphans(&caller, pid_alive)? {
        if f.store.requeue(id, &owner, crate::store::REQUEUE_ORPHAN)? {
            if let Some(t) = f.store.task(id)? {
                crate::git::clear_recorded_overlay(&t.worktree);
            }
            eprintln!("requeued task {id}: its previous worker exited");
        }
    }
    for (id, owner) in f.store.orphan_jobs(&caller, pid_alive)? {
        crate::job::recover_interrupted(f, id, &owner)?;
    }
    Ok(())
}

/// The double-signal abort: this worker's own work goes back in the queue.
fn requeue_aborted(f: &Forge, ids: &mut Vec<i64>, job_ids: &mut Vec<i64>) -> Result<()> {
    let own = crate::store::Owner::this_process();
    for id in ids.drain(..) {
        f.store.requeue(id, &own, crate::store::REQUEUE_ABORT)?;
    }
    for id in job_ids.drain(..) {
        crate::job::recover_interrupted(f, id, &own)?;
    }
    Ok(())
}

/// `worker.pid`: the newest worker's pid and the path it was launched by.
fn log_pass_error(result: Result<()>) -> bool {
    if let Err(error) = result {
        eprintln!("worker pass failed; retrying: {error:#}");
        true
    } else {
        false
    }
}

fn prepare_claim(f: &Forge) -> Result<Option<Vec<i64>>> {
    if let Some(msg) = day_budget_reached(f)? {
        eprintln!("{msg}; {} task(s) left queued", f.store.queued_count()?);
        return Ok(None);
    }
    for t in f.store.release_dependents()? {
        eprintln!("task {t} unblocked: its dependencies succeeded");
    }
    engine::settle_ready_initiatives(f)?;
    for (t, d, why) in f.store.block_dependents()? {
        eprintln!("task {t} blocked: {why} (task {d})");
        if let Some(task) = f.store.task(t)? {
            crate::audience::emit_ended(f, &task)?;
        }
    }
    Ok(Some(held_initiatives(f)?))
}

fn write_pid_file(paths: &Paths, pid: i64) {
    let exe = crate::binary::launch_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    // Line 2: the identity no reused pid shares (start time, or random id).
    let id = crate::store::start_of(pid).unwrap_or_default();
    let _ = std::fs::write(
        paths.home.join("worker.pid"),
        format!("{pid} {exe}\n{id}\n"),
    );
}

/// Sweep the proxy directories of workers that are gone (a live one's, a
/// draining predecessor's, is never touched); the guard removes this
/// worker's own at exit.
fn claim_egress_dir() -> crate::egress::OwnDirGuard {
    // Nothing of this process has made its directory yet, so one under its
    // pid is a dead worker's whose pid was reused.
    crate::egress::remove_own_dir();
    let swept = crate::egress::sweep_dead_in_run_root();
    if swept > 0 {
        eprintln!("egress: swept {swept} proxy director(ies) of dead pids");
    }
    crate::egress::OwnDirGuard
}

pub async fn work(mut f: Arc<Forge>, opts: WorkOpts) -> Result<()> {
    let mut shutdown = Shutdown::install();
    crate::egress::raise_nofile_limit();
    let _own_egress_dir = claim_egress_dir();
    // SIGHUP: re-read `config.toml` before the next claim, whatever its
    // mtime says (`crate::reload`).
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .expect("SIGHUP handler");
    let mut reloader = crate::reload::Reloader::start(&f);
    let mut hup = false;
    recover_orphans(&f, true)?;
    let pid = std::process::id() as i64;
    write_pid_file(&f.paths, pid);
    let mut succession = crate::successor::Succession::join(&f, &opts)?;
    let mut plugins = opts
        .poll
        .map(|_| crate::plugins::Supervisor::start(f.clone()));
    let mut capacity = capacity::Claims::default();
    let mut slots = capacity::slots(&f, &opts);
    let mut running: JoinSet<WorkResult> = JoinSet::new();
    let mut ids: Vec<i64> = Vec::new();
    let mut job_ids: Vec<i64> = Vec::new();
    let (mut done, mut ok) = (0u32, 0u32);
    let (mut jobs_done, mut jobs_ok) = (0u32, 0u32);
    let mut stopping = false;
    let mut env_error: Option<anyhow::Error> = None;
    let mut claimed = 0u32;
    let mut hold_until: Option<i64> = None;
    let mut last_idle: Option<String> = None;
    let mut refusals = RefusalLog::default();

    loop {
        // Config reloads between claims: what is claimed from here on runs
        // on the new config; what already runs keeps the `Forge` it holds.
        if !stopping && let Some(next) = reloader.check(&f, std::mem::take(&mut hup)) {
            if let Some(p) = &plugins {
                p.reload(next.clone());
            }
            f = next;
        }
        let mut superseded = false;
        let pass: Result<()> = async {
            recover_orphans(&f, false)?;
            superseded = succession.superseded(&f, &mut plugins, stopping).await?;
            if !stopping && succession.stop_requested().await {
                stopping = true;
                eprintln!(
                    "stopping: the unit has a stop job; {} running attempt(s) will finish",
                    running.len()
                );
            }
            run_ticks(&f, &mut refusals, superseded, stopping).await?;

            // Fill free slots, re-reading what the other workers hold.
            slots = free_slots(&f, pid, capacity::refresh(&f, &opts, &mut capacity)?);
            while !stopping
                && !superseded
                && env_error.is_none()
                && running.len() < slots
                && opts.max_tasks.is_none_or(|m| claimed < m)
            {
                crate::login_hold::probe_due(&f).await;
                let Some(held) = prepare_claim(&f)? else {
                    stopping = true;
                    break;
                };
                for line in new_holds(&f, &held) {
                    eprintln!("{line}");
                }
                if let Some(t) = claim::task(&f, &opts, pid, &held, |t| {
                    provider_is_held(&f, t) || intake_is_held(&f, t)
                })? {
                    hold_until = None;
                    claimed += 1;
                    eprintln!(
                        "======== task {} starting ({} queued, {} running)",
                        t.id,
                        f.store.queued_count().unwrap_or(0),
                        running.len() + 1
                    );
                    ids.push(t.id);
                    let fc = f.clone();
                    running.spawn(async move { WorkResult::Task(t.id, drive(fc, t.id).await) });
                } else if let Some(j) = claim::job(&f, &opts)? {
                    // A job carries no provider or initiative hold (it runs
                    // no directive step yet), so it is claimed only once
                    // every queued task has already been tried this pass.
                    hold_until = None;
                    claimed += 1;
                    eprintln!(
                        "======== job {} starting ({} queued, {} running)",
                        j.id,
                        f.store.queued_jobs().map_or(0, |jobs| jobs.len()),
                        running.len() + 1
                    );
                    job_ids.push(j.id);
                    let fc = f.clone();
                    running.spawn(async move { WorkResult::Job(j.id, job::drive(fc, j.id).await) });
                } else {
                    // Nothing claimable: only a provider cap is a hold worth waiting out.
                    if let Some((msg, until)) = tightest_provider_hold(&f, &held)? {
                        if hold_until != Some(until) {
                            eprintln!("{msg}");
                        }
                        hold_until = Some(until);
                    } else {
                        hold_until = None;
                        announce_idle(&f, &held, &mut last_idle);
                    }
                    break;
                }
            }

            Ok(())
        }
        .await;
        let pass_failed = log_pass_error(pass);

        if running.is_empty() {
            if superseded {
                eprintln!("a newer release claims; nothing left to finish, exiting");
                break;
            }
            let exhausted = opts.max_tasks.is_some_and(|m| claimed >= m);
            // A held window with work waiting: sleep until the reset (or the
            // poll interval), even in --once mode, which means "drain".
            let held = hold_until.filter(|_| {
                !stopping
                    && env_error.is_none()
                    && !exhausted
                    && f.store.queued_count().unwrap_or(0) > 0
            });
            match (held, opts.poll.or_else(|| pass_failed.then_some(10))) {
                (Some(until), poll) => {
                    let wait = (until - unix_now()).max(1) as u64;
                    let wait = poll.map_or(wait, |p| wait.min(p));
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(wait)) => continue,
                        _ = hangup.recv() => { hup = true; continue }
                        _ = shutdown.recv() => { eprintln!("stopping"); break }
                    }
                }
                (None, Some(secs)) if !stopping && env_error.is_none() && !exhausted => {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(secs)) => continue,
                        _ = hangup.recv() => { hup = true; continue }
                        _ = shutdown.recv() => { eprintln!("stopping"); break }
                    }
                }
                _ => break,
            }
        }

        tokio::select! {
            // With something running and a slot free, wake on the poll
            // interval too, so a task queued (or released, or scheduled)
            // meanwhile is claimed now rather than when the running one
            // finishes. Before this branch the loop woke only on a join or
            // a signal: on 2026-09-19 two tasks sat queued beside one
            // running attempt and two free slots for an hour.
            _ = tokio::time::sleep(Duration::from_secs(opts.poll.unwrap_or(10))), if running.len() < slots || opts.poll.is_some() => {}
            Some(joined) = running.join_next() => {
                match joined {
                    Ok(WorkResult::Task(id, Ok(state))) => {
                        ids.retain(|&x| x != id);
                        done += 1;
                        if state == TaskState::Succeeded { ok += 1 }
                        eprintln!("======== task {id} {}\n", state.as_str());
                    }
                    Ok(WorkResult::Task(id, Err(e))) => {
                        ids.retain(|&x| x != id);
                        eprintln!("======== task {id} could not run: {e:#}");
                        env_error = Some(e);
                        stopping = true;
                    }
                    Ok(WorkResult::Job(id, state)) => {
                        // A job's own failure is recorded on the job
                        // (`job::drive` never returns an error); it never
                        // sets `env_error` or touches any task's state.
                        job_ids.retain(|&x| x != id);
                        jobs_done += 1;
                        if state == JobState::Ok { jobs_ok += 1 }
                        eprintln!("======== job {id} {}\n", state.as_str());
                    }
                    Err(join) => {
                        eprintln!("a task or job panicked: {join}");
                        stopping = true;
                    }
                }
            }
            _ = hangup.recv() => { hup = true; }
            _ = shutdown.recv() => {
                if !stopping {
                    stopping = true;
                    eprintln!("stopping: no new tasks or jobs; {} running attempt(s) will finish (signal again to abort them)", running.len());
                } else {
                    eprintln!("aborting {} running attempt(s) and requeueing their tasks and jobs", running.len());
                    running.abort_all();
                    while running.join_next().await.is_some() {}
                    requeue_aborted(&f, &mut ids, &mut job_ids)?;
                    break;
                }
            }
        }
    }

    if let Some(p) = plugins {
        p.stop().await;
    }
    let handover = succession.leave(&f).await;
    eprintln!("worked {done} task(s): {ok} succeeded, {} not", done - ok);
    if jobs_done > 0 {
        eprintln!(
            "worked {jobs_done} job(s): {jobs_ok} ok, {} not",
            jobs_done - jobs_ok
        );
    }
    match env_error {
        Some(e) => Err(e),
        None => handover,
    }
}

#[cfg(test)]
mod tests;
