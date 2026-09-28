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
use anyhow::{Result, bail};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;

mod schedule;
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
    match engine::run_task(f.clone(), id).await {
        Ok(TaskState::Blocked) => {
            // A question addressed to someone other than the operator
            // (an intake interview's contact, say) is not the
            // supervisor's to rule on: leave it for the channel plugin,
            // and never spend a supervisor attempt on it.
            let addressed_elsewhere = f
                .store
                .task(id)?
                .and_then(|t| crate::supervisor::addressed_elsewhere(&t));
            if let Some(note) = addressed_elsewhere {
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
            Ok(TaskState::Blocked)
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
            Ok(TaskState::Failed)
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

/// The tightest (soonest-resetting) hold among every queued, unblocked
/// task's own provider (the one `first_role` says its next agent step
/// will run under, or an arm it would be re-drawn to), when *none* of
/// them can be claimed right now; `None` as soon as one candidate can run
/// under a provider that is not held, since the caller can claim it
/// instead of waiting. The message names the provider.
fn tightest_provider_hold(f: &Forge, held_initiatives: &[i64]) -> Result<Option<(String, i64)>> {
    let mut tightest: Option<(String, i64)> = None;
    for t in f.store.queued_unblocked(held_initiatives)? {
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
    Ok(tightest)
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

/// Every initiative currently holding new claims: its budget is spent, or
/// its trailing run of same-rule failures reached its stop rule (see
/// docs/PROJECTS.md, "Stop rule and budget"). Only initiatives with a
/// queued task are worth checking.
pub(crate) fn held_initiatives(f: &Forge) -> Result<Vec<i64>> {
    let mut held = Vec::new();
    for id in f.store.initiatives_with_queued_tasks()? {
        if let Some(ini) = f.store.initiative(id)?
            && crate::view::initiative_hold(f, &ini)?.is_some()
        {
            held.push(id);
        }
    }
    Ok(held)
}

/// One line for each initiative that just entered `held` (an id not seen
/// in `announced` before), naming why and how many of its tasks are stuck
/// queued behind it; nothing for a hold already announced, so a slow poll
/// interval does not turn into a flood (see `work`, which prints whatever
/// this returns). `announced` drops an id as soon as it leaves `held`, so
/// a later, separate hold on the same initiative is announced again.
fn new_holds(f: &Forge, held: &[i64], announced: &mut HashSet<i64>) -> Vec<String> {
    let mut lines = Vec::new();
    for &id in held {
        if !announced.insert(id) {
            continue;
        }
        let Ok(Some(ini)) = f.store.initiative(id) else {
            continue;
        };
        let queued = f
            .store
            .initiative_tasks(id)
            .map(|ts| ts.iter().filter(|t| t.state == TaskState::Queued).count())
            .unwrap_or(0);
        if queued == 0 {
            continue;
        }
        let reason = crate::view::initiative_hold_reason(f, &ini)
            .ok()
            .flatten()
            .unwrap_or_else(|| "held".to_string());
        lines.push(format!(
            "initiative {id} held ({reason}): {queued} queued task(s) skipped"
        ));
    }
    announced.retain(|id| held.contains(id));
    lines
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
    for project in f.store.list_projects()? {
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

pub struct WorkOpts {
    pub jobs: usize,
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
/// on every claim-loop pass. Writes are guarded on the owner listed.
fn recover_orphans(f: &Forge, just_started: bool) -> Result<()> {
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
        eprintln!("task {t} unblocked: its dependencies landed or were withdrawn");
    }
    engine::settle_ready_initiatives(f)?;
    for (t, d, why) in f.store.block_dependents()? {
        eprintln!("task {t} blocked: {why} (task {d})");
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
    let jobs = opts.jobs.max(1);
    let mut slots = jobs;
    let mut running: JoinSet<WorkResult> = JoinSet::new();
    let mut ids: Vec<i64> = Vec::new();
    let mut job_ids: Vec<i64> = Vec::new();
    let (mut done, mut ok) = (0u32, 0u32);
    let (mut jobs_done, mut jobs_ok) = (0u32, 0u32);
    let mut stopping = false;
    let mut env_error: Option<anyhow::Error> = None;
    let mut claimed = 0u32;
    let mut hold_until: Option<i64> = None;
    let mut announced_holds: HashSet<i64> = HashSet::new();
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
            superseded = succession.superseded(&f, &mut plugins).await?;
            if !stopping && succession.stop_requested() {
                stopping = true;
                eprintln!(
                    "stopping: the unit has a stop job; {} running attempt(s) will finish",
                    running.len()
                );
            }
            run_ticks(&f, &mut refusals, superseded, stopping).await?;

            // Fill free slots, re-reading what the other workers hold.
            slots = slot_budget(jobs, f.store.running_elsewhere(pid, pid_alive).unwrap_or(0));
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
                for line in new_holds(&f, &held, &mut announced_holds) {
                    eprintln!("{line}");
                }
                if let Some(t) = f.store.claim_next(pid, &held, |t| {
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
                } else if let Some(j) = f.store.claim_next_job()? {
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
                    // Nothing claimable: either the queue is empty/blocked, or
                    // every queued candidate's own provider is at its cap.
                    // Only the latter is a hold worth waiting out.
                    if let Some((msg, until)) = tightest_provider_hold(&f, &held)? {
                        if f.store.queued_count()? > 0 && hold_until != Some(until) {
                            eprintln!("{msg}; holding, {} task(s) queued", f.store.queued_count()?);
                        }
                        hold_until = Some(until);
                    } else {
                        hold_until = None;
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
    let handover = succession.leave(&f);
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
mod tests {
    #[test]
    fn a_worker_claims_only_the_slots_the_others_are_not_using() {
        use super::slot_budget;
        assert_eq!(slot_budget(4, 0), 4);
        assert_eq!(slot_budget(4, 2), 2);
        assert_eq!(slot_budget(4, 4), 0);
        assert_eq!(slot_budget(4, 9), 0, "never below zero");
        assert_eq!(slot_budget(1, 0), 1);
    }

    #[test]
    fn this_process_is_alive() {
        assert!(super::pid_alive(std::process::id() as i64));
    }

    #[test]
    fn pid_one_is_alive_even_when_it_is_not_ours_to_signal() {
        // As a non-root user kill(1, 0) fails with EPERM: alive all the same.
        assert!(super::pid_alive(1));
    }

    #[test]
    fn a_reaped_child_is_dead() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id() as i64;
        child.wait().unwrap();
        assert!(!super::pid_alive(pid));
    }

    #[test]
    fn zero_negative_and_out_of_range_pids_are_never_alive() {
        // kill(0, ..) and kill(-1, ..) address process groups; never ask.
        assert!(!super::pid_alive(0));
        assert!(!super::pid_alive(-1));
        assert!(!super::pid_alive(i64::from(i32::MAX) + 1));
    }

    /// A rate-limit sample older than the window it describes holds
    /// nothing, whatever reset time it names; a fresh one at its cap does.
    #[test]
    fn a_five_hour_sample_older_than_five_hours_never_holds() {
        use crate::store::RateLimitSample;
        let now = 1_800_000_000;
        let stale = RateLimitSample {
            seen_at: now - 5 * 3600 - 1,
            five_hour: Some(1.0),
            seven_day: None,
            five_hour_resets: Some(now + 6 * 3600),
            seven_day_resets: None,
        };
        assert_eq!(hold_from_sample(&stale, (0.9, 0.9), now), None);
        let fresh = RateLimitSample {
            seen_at: now - 60,
            ..stale
        };
        let (msg, until) = hold_from_sample(&fresh, (0.9, 0.9), now).unwrap();
        assert_eq!(until, now + 6 * 3600);
        assert!(msg.starts_with("rate window 5h at 100%"), "{msg}");
        let reset_passed = RateLimitSample {
            five_hour_resets: Some(now - 1),
            ..fresh
        };
        assert_eq!(hold_from_sample(&reset_passed, (0.9, 0.9), now), None);
    }

    use super::*;
    use crate::store::Store;

    /// A `Forge` over a fresh, empty store in a throwaway home: enough to
    /// resolve the builtin workflows `first_role` reads.
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

    fn task_on(workflow: &str) -> Task {
        Task {
            repo: "repo".into(),
            task: "do a thing".into(),
            base_branch: "main".into(),
            model: "sonnet".into(),
            max_turns: 10,
            max_attempts: 1,
            timeout_secs: 60,
            state: TaskState::Queued,
            created_at: crate::unix_now(),
            workflow: workflow.into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_hold_line_names_the_provider_once() {
        let window = "rate window 5h at 100% (cap 90%), resets in 71m";
        assert_eq!(
            named("openai", window),
            "openai: rate window 5h at 100% (cap 90%), resets in 71m"
        );
        let login = "openai: login refused since 01:00";
        assert_eq!(named("openai", login), login);
    }

    #[test]
    fn first_role_is_the_first_directive_steps_contract() {
        let (_dir, f) = fixture();
        assert_eq!(first_role(&f, &task_on("direct")), "code");
        assert_eq!(first_role(&f, &task_on("planned")), "plan");
    }

    #[test]
    fn first_role_falls_back_to_code_when_the_workflow_does_not_resolve() {
        let (_dir, f) = fixture();
        assert_eq!(first_role(&f, &task_on("no-such-workflow")), "code");
        let mut t = task_on("direct");
        t.actions_json = "not json".into();
        assert_eq!(first_role(&f, &t), "code");
    }

    #[test]
    fn first_role_is_the_next_undone_directive_on_a_resumed_task() {
        // `reviewed` resolves to setup, repo-map, code, review: seq 3 is
        // the code step. A record whose latest attempt at that seq
        // succeeded means the run already resumes past it (the same rule
        // `engine::resume_done` builds the cursor from), so the next
        // agent step, and the provider a claim is judged against, is the
        // review, not the code the task started on.
        let (_dir, f) = fixture();
        let mut t = task_on("reviewed");
        t.id = f.store.insert_task(&t).unwrap();
        assert_eq!(first_role(&f, &t), "code");
        f.store
            .insert_attempt(&crate::store::Attempt {
                task_id: t.id,
                attempt_no: 1,
                step_seq: 3,
                state: crate::store::AttemptState::Succeeded,
                started_at: crate::unix_now(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(first_role(&f, &t), "review");
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    /// A `fixture()` whose project `demo` has a landed repository holding
    /// one message-triggered run workflow per `(name, trigger table)`
    /// given, each with a noop step.
    fn message_fixture(triggers: &[(&str, &str)]) -> (tempfile::TempDir, Forge) {
        let (dir, f) = fixture();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".forge/workflows/actions")).unwrap();
        git_in(&repo, &["init", "-q", "-b", "main"]);
        git_in(&repo, &["config", "user.name", "Test"]);
        git_in(&repo, &["config", "user.email", "test@example.com"]);
        std::fs::write(repo.join("forge.toml"), "[checks]\nok = [\"true\"]\n").unwrap();
        std::fs::write(
            repo.join(".forge/workflows/actions/noop.toml"),
            "name = \"noop\"\nkind = \"operation\"\ndescription = \"nothing\"\nrun = [\"true\"]\n",
        )
        .unwrap();
        for (name, trigger) in triggers {
            std::fs::write(
                repo.join(format!(".forge/workflows/{name}.toml")),
                format!(
                    "name = \"{name}\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{{ action = \"noop\" }}]\n\n[trigger]\n{trigger}\n\n[assert]\nok = [\"true\"]\n"
                ),
            )
            .unwrap();
        }
        git_in(&repo, &["add", "-A"]);
        git_in(&repo, &["commit", "-qm", "init"]);
        f.store
            .create_project(&crate::store::Project {
                name: "demo".into(),
                purpose: "p".into(),
                created_at: 1,
                ..Default::default()
            })
            .unwrap();
        f.store
            .register_repo("demo", repo.to_str().unwrap(), None)
            .unwrap();
        (dir, f)
    }

    fn record(f: &Forge, direction: Direction, contact: &str, text: &str) -> Message {
        let id = f
            .store
            .insert_message(crate::store::InsertMessage {
                project: "demo",
                channel: "signal",
                contact,
                direction,
                text,
                task_id: None,
            })
            .unwrap();
        f.store.message(id).unwrap().unwrap()
    }

    #[tokio::test]
    async fn a_matching_message_starts_one_queued_job_with_the_message_as_input() {
        let (_dir, f) = message_fixture(&[("quote", "on = \"message\"\ncontact = \"alice\"")]);
        let m = record(&f, Direction::In, "alice", "a quote please");
        let started = message_triggers(&f, &m).await;
        assert_eq!(started.len(), 1, "{started:?}");
        assert_eq!(started[0].0, "quote");
        let job = f.store.job(started[0].1).unwrap().unwrap();
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.trigger_kind, "message");
        assert_eq!(job.trigger_ref, m.id.to_string());
        assert_eq!(job.due_at, None);
        let input = std::fs::read_to_string(
            f.paths
                .worktrees
                .join(format!("job-{}-input/input.json", job.id)),
        )
        .unwrap();
        let input: serde_json::Value = serde_json::from_str(&input).unwrap();
        assert_eq!(
            input,
            serde_json::json!({
                "from": "alice",
                "text": "a quote please",
                "at": m.at,
                "channel": "signal",
                "message_id": m.id,
            })
        );
    }

    #[tokio::test]
    async fn the_same_message_recorded_twice_starts_one_job() {
        let (_dir, f) = message_fixture(&[("quote", "on = \"message\"\ncontact = \"*\"")]);
        let m = record(&f, Direction::In, "alice", "hi");
        assert_eq!(message_triggers(&f, &m).await.len(), 1);
        assert!(message_triggers(&f, &m).await.is_empty());
        assert_eq!(f.store.jobs(Some("demo"), None).unwrap().len(), 1);
        // A different message is a different cause.
        let m2 = record(&f, Direction::In, "alice", "hi");
        assert_eq!(message_triggers(&f, &m2).await.len(), 1);
        assert_eq!(f.store.jobs(Some("demo"), None).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_non_matching_contact_starts_nothing_and_a_star_matches_anyone() {
        let (_dir, f) = message_fixture(&[
            ("only-bob", "on = \"message\"\ncontact = \"bob\""),
            ("anyone", "on = \"message\"\ncontact = \"*\""),
            ("tick", "on = \"schedule\"\ncron = \"* * * * *\""),
            ("by-hand", "on = \"manual\""),
        ]);
        let m = record(&f, Direction::In, "alice", "hi");
        let started = message_triggers(&f, &m).await;
        assert_eq!(
            started.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["anyone"]
        );
    }

    #[tokio::test]
    async fn an_outbound_message_fires_nothing() {
        let (_dir, f) = message_fixture(&[("anyone", "on = \"message\"\ncontact = \"*\"")]);
        let m = record(&f, Direction::Out, "alice", "on it");
        assert!(message_triggers(&f, &m).await.is_empty());
        assert!(f.store.jobs(Some("demo"), None).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_triggers_delay_makes_the_job_scheduled_from_the_messages_own_time() {
        let (_dir, f) =
            message_fixture(&[("later", "on = \"message\"\ncontact = \"*\"\ndelay = \"1h\"")]);
        let m = record(&f, Direction::In, "alice", "hi");
        let started = message_triggers(&f, &m).await;
        let job = f.store.job(started[0].1).unwrap().unwrap();
        assert_eq!(job.state, JobState::Scheduled);
        assert_eq!(job.due_at, Some(m.at + 3600));
    }

    #[tokio::test]
    async fn a_project_without_a_repository_fires_nothing() {
        let (_dir, f) = fixture();
        f.store
            .create_project(&crate::store::Project {
                name: "demo".into(),
                purpose: "p".into(),
                created_at: 1,
                ..Default::default()
            })
            .unwrap();
        let m = record(&f, Direction::In, "alice", "hi");
        assert!(message_triggers(&f, &m).await.is_empty());
    }

    /// Fire `demo`'s webhook `name` the way `forge job fire` does after its
    /// token check: resolve the workflow, then start the job.
    async fn fire(f: &Forge, name: &str, key: &str, input: &str) -> Result<(i64, bool)> {
        let (workflow, wf, source, sha) = webhook_workflow(f, "demo", name).await?;
        job::start_webhook(crate::job::StartWebhook {
            f,
            project: "demo",
            workflow: &workflow,
            landed_sha: &sha,
            wf: &wf,
            source,
            trigger_ref: key,
            input_text: input,
        })
    }

    #[tokio::test]
    async fn a_webhook_starts_one_queued_job_with_the_body_as_input_and_its_key_as_the_ref() {
        let (_dir, f) = message_fixture(&[
            ("ship", "on = \"webhook\"\nname = \"orders\""),
            ("other", "on = \"webhook\"\nname = \"refunds\""),
            ("by-message", "on = \"message\"\ncontact = \"*\""),
        ]);
        let (id, started) = fire(&f, "orders", "delivery-1", r#"{"order":"17"}"#)
            .await
            .unwrap();
        assert!(started);
        let job = f.store.job(id).unwrap().unwrap();
        assert_eq!(job.workflow, "ship");
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.trigger_kind, "webhook");
        assert_eq!(job.trigger_ref, "delivery-1");
        let input =
            std::fs::read_to_string(f.paths.worktrees.join(format!("job-{id}-input/input.json")))
                .unwrap();
        assert_eq!(input, r#"{"order":"17"}"#);
        assert_eq!(f.store.jobs(Some("demo"), None).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_delivery_fired_again_returns_its_job_and_starts_no_second() {
        let (_dir, f) = message_fixture(&[("ship", "on = \"webhook\"\nname = \"orders\"")]);
        let (first, started) = fire(&f, "orders", "k", "{}").await.unwrap();
        assert!(started);
        let (again, started) = fire(&f, "orders", "k", r#"{"different":"body"}"#)
            .await
            .unwrap();
        assert!(!started);
        assert_eq!(again, first);
        assert_eq!(f.store.jobs(Some("demo"), None).unwrap().len(), 1);
        // Another key is another delivery.
        let (second, started) = fire(&f, "orders", "k2", "{}").await.unwrap();
        assert!(started && second != first);
        assert_eq!(f.store.jobs(Some("demo"), None).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn the_unique_index_backs_the_key_when_two_deliveries_race() {
        let (_dir, f) = message_fixture(&[("ship", "on = \"webhook\"\nname = \"orders\"")]);
        let (id, _) = fire(&f, "orders", "k", "{}").await.unwrap();
        let mut dup = f.store.job(id).unwrap().unwrap();
        dup.id = 0;
        assert!(f.store.create_job(&dup).is_err(), "jobs_webhook_ref");
        // A retry:N requeue carries the ref and is exempt.
        dup.retry_count = 1;
        assert!(f.store.create_job(&dup).is_ok());
    }

    #[tokio::test]
    async fn a_webhook_nothing_or_two_workflows_claim_is_an_error() {
        let (_dir, f) = message_fixture(&[
            ("a", "on = \"webhook\"\nname = \"twice\""),
            ("b", "on = \"webhook\"\nname = \"twice\""),
            ("m", "on = \"message\"\ncontact = \"orders\""),
        ]);
        let none = webhook_workflow(&f, "demo", "orders").await.unwrap_err();
        assert!(none.to_string().contains("no run workflow"), "{none}");
        let two = webhook_workflow(&f, "demo", "twice").await.unwrap_err();
        assert!(two.to_string().contains("a, b"), "{two}");
    }

    #[tokio::test]
    async fn a_webhook_body_that_is_not_a_json_object_starts_nothing() {
        let (_dir, f) = message_fixture(&[("ship", "on = \"webhook\"\nname = \"orders\"")]);
        assert!(fire(&f, "orders", "k", "not json").await.is_err());
        assert!(fire(&f, "orders", "k", "[1,2]").await.is_err());
        assert!(f.store.jobs(Some("demo"), None).unwrap().is_empty());
        // An empty body is an empty object.
        let (id, _) = fire(&f, "orders", "k", "  ").await.unwrap();
        let input =
            std::fs::read_to_string(f.paths.worktrees.join(format!("job-{id}-input/input.json")))
                .unwrap();
        assert_eq!(input, "{}");
    }

    #[tokio::test]
    async fn a_webhook_triggers_delay_makes_the_job_scheduled() {
        let (_dir, f) = message_fixture(&[(
            "later",
            "on = \"webhook\"\nname = \"orders\"\ndelay = \"1h\"",
        )]);
        let before = unix_now();
        let (id, _) = fire(&f, "orders", "k", "{}").await.unwrap();
        let job = f.store.job(id).unwrap().unwrap();
        assert_eq!(job.state, JobState::Scheduled);
        assert!(job.due_at.unwrap() >= before + 3600);
    }

    #[tokio::test]
    async fn a_schedule_tick_refused_by_per_day_records_the_refusal_once() {
        let (dir, f) = message_fixture(&[("nightly", "on = \"schedule\"\ncron = \"* * * * *\"")]);
        let repo = dir.path().join("repo");
        let file = repo.join(".forge/workflows/nightly.toml");
        let mut text = std::fs::read_to_string(&file).unwrap();
        text.push_str("\n[limits]\nbudget_usd = 1.0\nper_day = 1\non_failure = \"drop\"\n");
        std::fs::write(&file, text).unwrap();
        git_in(&repo, &["commit", "-aqm", "cap"]);
        f.store
            .create_job(&crate::store::Job {
                project: "demo".into(),
                workflow: "nightly".into(),
                trigger_kind: "manual".into(),
                state: JobState::Ok,
                started_at: unix_now() - 3600,
                verdict_json: "[]".into(),
                ..Default::default()
            })
            .unwrap();
        let mut log = RefusalLog::default();
        for _ in 0..3 {
            let runs = tick_run_workflows(&f).await.unwrap();
            schedule_tick(&f, &runs, &mut log).await.unwrap();
        }
        assert_eq!(f.store.jobs(Some("demo"), None).unwrap().len(), 1);
        let rows = f.store.schedule_refusals().unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            (rows[0].project.as_str(), rows[0].workflow.as_str()),
            ("demo", "nightly")
        );
        assert_eq!(Some(rows[0].since), log.since("demo/nightly"));
        let next = rows[0].next_allowed.expect("the window rolls");
        assert!((next - (unix_now() + 23 * 3600)).abs() < 300, "{next}");
        assert!(rows[0].reason.contains("per_day limit is 1"), "{rows:?}");
    }

    /// One pass of the event tick, as the poll loop runs it.
    async fn tick_events(f: &Forge) {
        let runs = tick_run_workflows(f).await.unwrap();
        event_tick(f, &runs).await.unwrap();
    }

    /// Insert a task for `project` and emit its `task_done` with `state`.
    fn finish_task(f: &Forge, project: &str, state: &str) -> i64 {
        let mut t = task_on("direct");
        t.project = Some(project.into());
        t.id = f.store.insert_task(&t).unwrap();
        f.store.update_task(&t).unwrap();
        f.report.emit(
            t.id,
            Event::TaskDone {
                state,
                attempts: 1,
                cost: 0.0,
                reason: "",
                branch: "b",
                pushed: false,
                compare: None,
                remove_cmd: "",
            },
        );
        t.id
    }

    fn emit_job_finished(f: &Forge, workflow: &str, job_id: i64) {
        f.report.emit(
            0,
            Event::JobFinished {
                project: "demo",
                workflow,
                job_id,
                state: "ok",
                cost_usd: 0.0,
            },
        );
    }

    fn event_jobs(f: &Forge) -> Vec<crate::store::Job> {
        f.store
            .jobs(Some("demo"), None)
            .unwrap()
            .into_iter()
            .filter(|j| j.trigger_kind == "event")
            .collect()
    }

    #[tokio::test]
    async fn a_superseded_or_stopping_pass_fires_no_tick() {
        let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
        let mut log = RefusalLog::default();
        for (superseded, stopping) in [(true, false), (false, true), (true, true)] {
            let ran = run_ticks(&f, &mut log, superseded, stopping).await.unwrap();
            assert!(!ran);
            assert_eq!(f.store.event_cursor("demo", "on-done").unwrap(), None);
        }
        assert!(run_ticks(&f, &mut log, false, false).await.unwrap());
        assert!(f.store.event_cursor("demo", "on-done").unwrap().is_some());
    }

    #[tokio::test]
    async fn a_failing_event_tick_does_not_end_the_pass() {
        let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
        // A directory where the log should be: the event tick errors.
        let log_path = f.paths.home.join("events.jsonl");
        let _ = std::fs::remove_file(&log_path);
        std::fs::create_dir_all(&log_path).unwrap();
        let runs = tick_run_workflows(&f).await.unwrap();
        assert!(event_tick(&f, &runs).await.is_err());
        let mut refusals = RefusalLog::default();
        assert!(run_ticks(&f, &mut refusals, false, false).await.unwrap());
    }

    #[tokio::test]
    async fn two_start_events_on_one_event_yield_one_job_and_no_error() {
        let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
        let runs = tick_run_workflows(&f).await.unwrap();
        let run = &runs[0];
        let start = || {
            job::start_event(crate::job::StartEvent {
                f: &f,
                project: "demo",
                workflow: "on-done",
                landed_sha: &run.landed_sha,
                wf: &run.wf,
                source: run.source,
                offset: "0:42",
                at: unix_now(),
                input: "{}",
            })
        };
        let first = start().unwrap();
        assert!(first.is_some());
        assert_eq!(start().unwrap(), None);
        assert_eq!(event_jobs(&f).len(), 1);
    }

    #[tokio::test]
    async fn a_matching_event_starts_one_queued_job_with_the_event_as_input_and_its_offset_as_the_ref()
     {
        let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
        tick_events(&f).await; // first sight: starts at the end of the log
        let before = std::fs::metadata(f.paths.home.join("events.jsonl"))
            .map(|m| m.len())
            .unwrap_or(0);
        let task = finish_task(&f, "demo", "succeeded");
        tick_events(&f).await;
        let jobs = event_jobs(&f);
        assert_eq!(jobs.len(), 1, "{jobs:?}");
        let job = &jobs[0];
        assert_eq!(job.workflow, "on-done");
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.trigger_ref, format!("0:{before}"));
        assert_eq!(job.due_at, None);
        let input = std::fs::read_to_string(
            f.paths
                .worktrees
                .join(format!("job-{}-input/input.json", job.id)),
        )
        .unwrap();
        let input: serde_json::Value = serde_json::from_str(&input).unwrap();
        assert_eq!(input["type"], "task_done");
        assert_eq!(input["state"], "succeeded");
        assert_eq!(input["task"], task);
        // Ticking again, or "restarting" (nothing in memory), starts no second.
        tick_events(&f).await;
        assert_eq!(event_jobs(&f).len(), 1);
    }

    #[tokio::test]
    async fn a_workflow_first_seen_after_events_happened_is_not_backfilled() {
        let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
        finish_task(&f, "demo", "succeeded");
        tick_events(&f).await;
        assert!(event_jobs(&f).is_empty());
        finish_task(&f, "demo", "failed");
        tick_events(&f).await;
        assert_eq!(event_jobs(&f).len(), 1);
    }

    #[tokio::test]
    async fn only_events_of_the_declared_type_for_the_project_start_a_job() {
        let (_dir, f) = message_fixture(&[
            ("on-done", "on = \"event\"\ntype = \"task_done\""),
            ("on-deploy", "on = \"event\"\ntype = \"deploy_finished\""),
            ("by-hand", "on = \"manual\""),
        ]);
        f.store
            .create_project(&crate::store::Project {
                name: "other".into(),
                purpose: "p".into(),
                created_at: 1,
                ..Default::default()
            })
            .unwrap();
        tick_events(&f).await;
        finish_task(&f, "other", "succeeded"); // another project's task
        f.report.emit(0, Event::Note { text: "nothing" });
        tick_events(&f).await;
        assert!(event_jobs(&f).is_empty());

        f.report.emit(
            0,
            Event::DeployFinished {
                project: "other",
                target: "prod",
                sha: "abc",
                ok: true,
                rolled_back_to: None,
            },
        );
        tick_events(&f).await;
        assert!(event_jobs(&f).is_empty(), "another project's deploy");
        f.report.emit(
            0,
            Event::DeployFinished {
                project: "demo",
                target: "prod",
                sha: "abc",
                ok: true,
                rolled_back_to: None,
            },
        );
        tick_events(&f).await;
        let jobs = event_jobs(&f);
        assert_eq!(jobs.len(), 1, "{jobs:?}");
        assert_eq!(jobs[0].workflow, "on-deploy");
    }

    #[tokio::test]
    async fn a_jobs_own_finish_never_starts_the_workflow_that_ran_it() {
        let (_dir, f) = message_fixture(&[
            ("again", "on = \"event\"\ntype = \"job_finished\""),
            ("watcher", "on = \"event\"\ntype = \"job_finished\""),
        ]);
        tick_events(&f).await;
        emit_job_finished(&f, "again", 41);
        tick_events(&f).await;
        let jobs = event_jobs(&f);
        assert_eq!(jobs.len(), 1, "{jobs:?}");
        assert_eq!(jobs[0].workflow, "watcher", "only the other workflow fires");
        // The watcher's own job finishing starts `again`, not itself.
        emit_job_finished(&f, "watcher", jobs[0].id);
        tick_events(&f).await;
        let mut names: Vec<_> = event_jobs(&f).into_iter().map(|j| j.workflow).collect();
        names.sort();
        assert_eq!(names, vec!["again", "watcher"]);
    }

    #[tokio::test]
    async fn the_unique_index_backs_the_offset_when_a_cursor_write_is_lost() {
        let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
        tick_events(&f).await;
        finish_task(&f, "demo", "succeeded");
        tick_events(&f).await;
        let job = event_jobs(&f).remove(0);
        // The crash between the job and the cursor: the cursor is back at 0.
        f.store.set_event_cursor("demo", "on-done", "0:0").unwrap();
        tick_events(&f).await;
        assert_eq!(event_jobs(&f).len(), 1);
        let mut dup = job;
        dup.id = 0;
        assert!(f.store.create_job(&dup).is_err(), "jobs_event_ref");
        dup.retry_count = 1;
        assert!(f.store.create_job(&dup).is_ok(), "a retry is exempt");
    }

    #[tokio::test]
    async fn event_generations_drain_the_tail_and_deduplicate_only_within_a_generation() {
        let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
        tick_events(&f).await;
        let path = f.paths.home.join("events.jsonl");
        let line = "{\"type\":\"task_done\",\"project\":\"demo\",\"ts\":1}\n";
        crate::report::log::append(&path, "{}\n", u64::MAX).unwrap();
        crate::report::log::append(&path, line, 0).unwrap();
        let saved = f.store.event_cursor("demo", "on-done").unwrap().unwrap();
        tick_events(&f).await;
        assert_eq!(event_jobs(&f).len(), 1);
        f.store.set_event_cursor("demo", "on-done", &saved).unwrap();
        crate::report::log::append(&path, line, 0).unwrap();
        tick_events(&f).await;
        let jobs = event_jobs(&f);
        assert_eq!(jobs.len(), 2);
        let refs: Vec<_> = jobs
            .iter()
            .map(|j| j.trigger_ref.split_once(':').unwrap())
            .collect();
        assert_ne!(refs[0].0, refs[1].0);
        assert_eq!(refs[0].1, refs[1].1);
        tick_events(&f).await;
        assert_eq!(event_jobs(&f).len(), 2);
    }

    #[tokio::test]
    async fn a_log_that_rolled_is_read_again_from_its_start() {
        let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
        tick_events(&f).await;
        for _ in 0..3 {
            f.report.emit(
                0,
                Event::Note {
                    text: "padding padding",
                },
            );
        }
        tick_events(&f).await;
        let log = f.paths.home.join("events.jsonl");
        let cursor = f.store.event_cursor("demo", "on-done").unwrap().unwrap();
        assert_eq!(
            cursor.split_once(':').unwrap().1.parse::<u64>().unwrap(),
            std::fs::metadata(&log).unwrap().len()
        );
        // The log rolls: a new, shorter file.
        std::fs::remove_file(&log).unwrap();
        finish_task(&f, "demo", "succeeded");
        assert!(
            std::fs::metadata(&log).unwrap().len()
                < cursor.split_once(':').unwrap().1.parse::<u64>().unwrap()
        );
        tick_events(&f).await;
        assert_eq!(event_jobs(&f).len(), 1);
        assert_eq!(event_jobs(&f)[0].trigger_ref, "0:0");
    }

    /// A held initiative with a queued task is announced the first time
    /// `new_holds` sees it, never again while the hold continues (even
    /// across many polls), and again once it leaves `held` and re-enters
    /// (docs/PROJECTS.md, "Stop rule and budget"): the claim loop calls
    /// this every poll, so this is what keeps a slow poll from spamming.
    #[test]
    fn new_holds_announces_a_held_initiative_once_per_hold() {
        let (_dir, f) = fixture();
        f.store
            .create_project(&crate::store::Project {
                name: "demo".into(),
                purpose: "p".into(),
                created_at: 1,
                ..Default::default()
            })
            .unwrap();
        let ini_id = f
            .store
            .create_initiative(&crate::store::Initiative {
                project: "demo".into(),
                outcome: "o".into(),
                budget_usd: Some(0.0),
                stop_after_same_rule: 3,
                created_at: 1,
                ..Default::default()
            })
            .unwrap();
        let mut t = task_on("direct");
        t.project = Some("demo".into());
        t.initiative = Some(ini_id);
        t.id = f.store.insert_task(&t).unwrap();
        f.store.update_task(&t).unwrap();

        let mut announced = HashSet::new();
        let held = vec![ini_id];
        let first = new_holds(&f, &held, &mut announced);
        assert_eq!(first.len(), 1);
        assert!(
            first[0].contains(&format!("initiative {ini_id}")),
            "{first:?}"
        );
        assert!(first[0].contains("budget"), "{first:?}");

        // Same hold, three more polls: nothing new to say.
        for _ in 0..3 {
            assert!(new_holds(&f, &held, &mut announced).is_empty());
        }

        // The hold lifts (no longer in `held`), then recurs: announced again.
        assert!(new_holds(&f, &[], &mut announced).is_empty());
        let again = new_holds(&f, &held, &mut announced);
        assert_eq!(again.len(), 1);
    }
}
