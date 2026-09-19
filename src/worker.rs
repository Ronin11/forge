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
use crate::store::{JobState, Task, TaskState};
use crate::unix_now;
use crate::workflows;
use crate::{config, git};
use anyhow::Result;
use croner::Cron;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;

/// What one slot of the worker's `--jobs` cap finished running: a task
/// (`src/engine.rs`) or a job (`src/job.rs`) — the worker claims both from
/// the same queue of free slots (docs/JOBS.md step 1d).
enum WorkResult {
    Task(i64, Result<TaskState>),
    Job(i64, JobState),
}

pub fn pid_alive(pid: i64) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// The worker as `worker.pid` says: pid, its binary, whether it is alive,
/// and whether that binary was rebuilt underneath it since it started.
pub struct WorkerStatus {
    pub pid: i64,
    pub exe: String,
    pub running: bool,
    pub stale: bool,
}

pub fn worker_status(paths: &Paths) -> Option<WorkerStatus> {
    let text = std::fs::read_to_string(paths.home.join("worker.pid")).ok()?;
    let mut it = text.split_whitespace();
    let pid: i64 = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let exe = it.next().unwrap_or("").to_string();
    let running = pid > 0 && pid_alive(pid);
    let stale = running
        && std::fs::read_link(format!("/proc/{pid}/exe"))
            .map(|p| p.to_string_lossy().ends_with(" (deleted)"))
            .unwrap_or(false);
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
            }
            Ok(TaskState::Failed)
        }
        Err(Fault::Env(e)) => {
            f.store.requeue(id, "worker environment error")?;
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

/// `provider`'s subscription window at or over its own cap, by the latest
/// sample any attempt on it recorded: the message and the unix second the
/// hold ends. A window whose reset time has passed no longer holds
/// anything. Each provider has its own samples and its own caps (see
/// `agent::Provider::five_hour_max`/`seven_day_max`), so a provider with no
/// samples of its own is never held by another's.
pub fn window_hold(f: &Forge, provider: &str) -> Result<Option<(String, i64)>> {
    let Some(s) = f.store.latest_rate_limit(provider)? else {
        return Ok(None);
    };
    let (five_hour_max, seven_day_max) = f
        .providers
        .get(provider)
        .map(|p| (p.five_hour_max, p.seven_day_max))
        .unwrap_or((f.budget.five_hour_max, f.budget.seven_day_max));
    let now = unix_now();
    let mut hold: Option<(String, i64)> = None;
    for (name, util, resets, cap) in [
        ("5h", s.five_hour, s.five_hour_resets, five_hour_max),
        ("7d", s.seven_day, s.seven_day_resets, seven_day_max),
    ] {
        let (Some(u), Some(r)) = (util, resets) else {
            continue;
        };
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
    Ok(hold)
}

/// The role the task's *next agent step* will actually run under: the
/// contract of the first directive step of its resolved workflow (see
/// `engine::run_task`, which resolves the same way at start). Falls back
/// to "code" on any failure to resolve (unknown/broken workflow, no
/// directive step): a provider that fails to resolve is never held here,
/// the real error surfaces when the task actually runs.
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
    resolved
        .steps
        .into_iter()
        .find(|s| s.action.kind == workflows::Kind::Directive)
        .map(|s| s.action.contract.as_str().to_string())
        .unwrap_or_else(|| "code".into())
}

/// Whether the provider that `t`'s next agent step will actually run
/// under (see `first_role`) is currently held.
fn provider_is_held(f: &Forge, t: &Task) -> bool {
    f.effective_provider(t, &first_role(f, t))
        .ok()
        .and_then(|p| window_hold(f, &p.name).ok().flatten())
        .is_some()
}

/// The tightest (soonest-resetting) hold among every queued, unblocked
/// task's own provider (the one `first_role` says its next agent step
/// will run under), when *none* of them can be claimed right now;
/// `None` as soon as one candidate's provider is not held, since the
/// caller can claim it instead of waiting.
fn tightest_provider_hold(f: &Forge, held_initiatives: &[i64]) -> Result<Option<(String, i64)>> {
    let mut tightest: Option<(String, i64)> = None;
    for t in f.store.queued_unblocked(held_initiatives)? {
        let role = first_role(f, &t);
        let Ok(provider) = f.effective_provider(&t, &role) else {
            return Ok(None);
        };
        match window_hold(f, &provider.name)? {
            None => return Ok(None),
            Some((msg, until)) => {
                if tightest.as_ref().is_none_or(|(_, u)| until < *u) {
                    tightest = Some((msg, until));
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

/// One line the first time an initiative enters `held` (an id not seen in
/// `announced` before), naming why and how many of its tasks are stuck
/// queued behind it; nothing on later passes while the hold continues, so
/// a slow poll interval does not turn into a flood. `announced` drops an
/// id as soon as it leaves `held`, so a later, separate hold on the same
/// initiative is announced again.
fn announce_new_holds(f: &Forge, held: &[i64], announced: &mut HashSet<i64>) {
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
        eprintln!("initiative {id} held ({reason}): {queued} queued task(s) skipped");
    }
    announced.retain(|id| held.contains(id));
}

/// One project's run workflow with a schedule trigger, resolved for this
/// tick: its cron and the unix second (`Job::trigger_ref`) its last
/// scheduled job recorded, or `None` when it has never started one.
struct Schedule {
    project: String,
    workflow: String,
    cron: Cron,
    last_ref: Option<i64>,
}

/// One schedule due this tick, at the slot it is due for.
struct Due {
    project: String,
    workflow: String,
    slot: i64,
}

/// Pure (docs/JOBS.md, "Triggers"): which of `schedules` are due at `now`,
/// and at which slot. A schedule is due when the latest cron occurrence at
/// or before `now` is newer than its `last_ref` — `None` counts as
/// "before everything", so a schedule that has never run is due at the
/// current slot the first time a tick sees it, never backfilled from
/// whenever the cron would first have matched. A gap of several missed
/// slots (the worker was down) still yields one `Due`: the latest
/// occurrence, never one per missed slot, so a restart cannot flood the
/// queue or re-fire an old one. Ticking again inside the same slot (the
/// poll interval is shorter than the cron's own granularity) yields
/// nothing once `last_ref` catches up to it.
fn due_schedules(now: i64, schedules: Vec<Schedule>) -> Vec<Due> {
    let Some(now_dt) = chrono::DateTime::from_timestamp(now, 0) else {
        return Vec::new();
    };
    schedules
        .into_iter()
        .filter_map(|s| {
            let slot = s
                .cron
                .find_previous_occurrence(&now_dt, true)
                .ok()?
                .timestamp();
            s.last_ref.is_none_or(|r| slot > r).then_some(Due {
                project: s.project,
                workflow: s.workflow,
                slot,
            })
        })
        .collect()
}

/// Every run workflow with a schedule trigger that resolves for `project`
/// right now: its own repository's `.forge/workflows/*.toml` at its
/// latest landed commit, and the operator's catalog, resolved by name the
/// same way `forge job start` resolves any other workflow — the
/// repository first, the catalog only for a name the repository does not
/// have there (docs/JOBS.md, "Where an automation lives"). A project with
/// no registered repository, or whose repository's `base_branch` has
/// never landed anything, contributes nothing. A broken workflow file
/// (the catalog's own, or one under this project's `.forge/workflows/`)
/// is noted on stderr and skipped rather than stopping the tick for every
/// other project.
async fn project_schedules(
    f: &Forge,
    project: &str,
) -> Vec<(String, workflows::Workflow, workflows::JobSource, String)> {
    let mut out = Vec::new();
    let repo = match f.store.first_repo(project) {
        Ok(Some(r)) => r,
        Ok(None) => return out,
        Err(e) => {
            eprintln!("schedule tick: {project}: {e:#}");
            return out;
        }
    };
    let repo_path = Path::new(&repo);
    let cfg = match config::load_working(repo_path).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("schedule tick: {project}: {e:#}");
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
            eprintln!("schedule tick: {project}: {e:#}");
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
        Err(e) => eprintln!("schedule tick: {e:#}"),
    }
    for name in names {
        match workflows::resolve_job_for_project(&f.paths.home, repo_path, &landed_sha, &name) {
            Ok((wf, _steps, source)) => out.push((name, wf, source, landed_sha.clone())),
            Err(e) => eprintln!("schedule tick: {project}/{name}: {e:#}"),
        }
    }
    out
}

/// The worker's schedule trigger (docs/JOBS.md, "Triggers" and "Build
/// order" step 3): for every project, every run workflow with `[trigger]
/// on = "schedule"` that resolves for it, queue one job per cron slot due
/// since its last scheduled job. Called once per pass of the poll loop
/// (`work`, below); cheap when no project has a schedule due, since
/// `due_schedules` alone decides what starts.
async fn schedule_tick(f: &Forge) -> Result<()> {
    let now = unix_now();
    let mut schedules = Vec::new();
    let mut resolved: HashMap<
        (String, String),
        (workflows::Workflow, workflows::JobSource, String),
    > = HashMap::new();
    for project in f.store.list_projects()? {
        for (name, wf, source, landed_sha) in project_schedules(f, &project.name).await {
            let Some(trigger) = wf.trigger.as_ref() else {
                continue;
            };
            if trigger.on != workflows::TriggerOn::Schedule {
                continue;
            }
            // `workflows::parse` already refused an unparseable cron at
            // load time (with the file and the line), so this always
            // parses; a defensive skip rather than a panic if it somehow
            // did not.
            let Some(cron) = trigger.cron.as_deref().and_then(|e| Cron::from_str(e).ok()) else {
                continue;
            };
            let last_ref = match f.store.last_scheduled_job(&project.name, &name) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("schedule tick: {}/{name}: {e:#}", project.name);
                    continue;
                }
            };
            schedules.push(Schedule {
                project: project.name.clone(),
                workflow: name.clone(),
                cron,
                last_ref,
            });
            resolved.insert((project.name.clone(), name), (wf, source, landed_sha));
        }
    }
    for due in due_schedules(now, schedules) {
        let Some((wf, source, landed_sha)) =
            resolved.get(&(due.project.clone(), due.workflow.clone()))
        else {
            continue;
        };
        match job::start_scheduled(
            f,
            &due.project,
            &due.workflow,
            landed_sha,
            wf,
            *source,
            due.slot,
        )
        .await
        {
            Ok(id) => eprintln!(
                "======== job {id} starting (schedule {} on {})",
                due.workflow, due.project
            ),
            Err(e) => eprintln!("schedule tick: {}/{}: {e:#}", due.project, due.workflow),
        }
    }
    Ok(())
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

pub async fn work(f: Arc<Forge>, opts: WorkOpts) -> Result<()> {
    let mut shutdown = Shutdown::install();
    for id in f.store.orphans(pid_alive)? {
        f.store.requeue(id, "previous worker exited")?;
        eprintln!("requeued task {id}: its previous worker exited");
    }
    let pid = std::process::id() as i64;
    let pid_file = f.paths.home.join("worker.pid");
    let _ = std::fs::write(
        &pid_file,
        format!(
            "{pid} {}\n",
            std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        ),
    );
    let plugins = crate::plugins::Supervisor::start(f.clone());
    let jobs = opts.jobs.max(1);
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

    loop {
        schedule_tick(&f).await?;

        // Fill free slots.
        while !stopping
            && env_error.is_none()
            && running.len() < jobs
            && opts.max_tasks.is_none_or(|m| claimed < m)
        {
            if let Some(msg) = day_budget_reached(&f)? {
                eprintln!("{msg}; {} task(s) left queued", f.store.queued_count()?);
                stopping = true;
                break;
            }
            for t in f.store.release_dependents()? {
                eprintln!("task {t} unblocked: its dependencies landed or were withdrawn");
            }
            for (t, d, why) in f.store.block_dependents()? {
                eprintln!("task {t} blocked: {why} (task {d})");
            }
            let held = held_initiatives(&f)?;
            announce_new_holds(&f, &held, &mut announced_holds);
            if let Some(t) = f.store.claim_next(pid, &held, |t| {
                provider_is_held(&f, t) || intake_is_held(&f, t)
            })? {
                hold_until = None;
                claimed += 1;
                eprintln!(
                    "======== task {} starting ({} queued, {} running)",
                    t.id,
                    f.store.queued_count()?,
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
                    f.store.queued_jobs()?.len(),
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

        if running.is_empty() {
            let exhausted = opts.max_tasks.is_some_and(|m| claimed >= m);
            // A held window with work waiting: sleep until the reset (or the
            // poll interval), even in --once mode, which means "drain".
            let held = hold_until.filter(|_| {
                !stopping
                    && env_error.is_none()
                    && !exhausted
                    && f.store.queued_count().unwrap_or(0) > 0
            });
            match (held, opts.poll) {
                (Some(until), poll) => {
                    let wait = (until - unix_now()).max(1) as u64;
                    let wait = poll.map_or(wait, |p| wait.min(p));
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(wait)) => continue,
                        _ = shutdown.recv() => { eprintln!("stopping"); break }
                    }
                }
                (None, Some(secs)) if !stopping && env_error.is_none() && !exhausted => {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(secs)) => continue,
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
            _ = tokio::time::sleep(Duration::from_secs(opts.poll.unwrap_or(10))), if running.len() < jobs => {}
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
            _ = shutdown.recv() => {
                if !stopping {
                    stopping = true;
                    eprintln!("stopping: no new tasks or jobs; {} running attempt(s) will finish (signal again to abort them)", running.len());
                } else {
                    eprintln!("aborting {} running attempt(s) and requeueing their tasks and jobs", running.len());
                    running.abort_all();
                    while running.join_next().await.is_some() {}
                    for id in ids.drain(..) {
                        f.store.requeue(id, "worker aborted by operator")?;
                    }
                    for id in job_ids.drain(..) {
                        f.store.requeue_job(id)?;
                    }
                    break;
                }
            }
        }
    }

    plugins.stop().await;
    eprintln!("worked {done} task(s): {ok} succeeded, {} not", done - ok);
    if jobs_done > 0 {
        eprintln!(
            "worked {jobs_done} job(s): {jobs_ok} ok, {} not",
            jobs_done - jobs_ok
        );
    }
    match env_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
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

    /// A minute-aligned unix second: `Cron::find_previous_occurrence`
    /// works in whole seconds, so every fixture below starts from one to
    /// keep "the minute" unambiguous.
    fn minute_boundary() -> i64 {
        let t = unix_now();
        t - (t % 60)
    }

    fn every_minute() -> Cron {
        Cron::from_str("* * * * *").unwrap()
    }

    fn schedule(last_ref: Option<i64>) -> Schedule {
        Schedule {
            project: "equitizr".into(),
            workflow: "snapshot".into(),
            cron: every_minute(),
            last_ref,
        }
    }

    #[test]
    fn due_once_at_the_minute() {
        let now = minute_boundary();
        let due = due_schedules(now, vec![schedule(None)]);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].project, "equitizr");
        assert_eq!(due[0].workflow, "snapshot");
        assert_eq!(due[0].slot, now);
    }

    #[test]
    fn not_due_twice_in_the_same_minute() {
        let now = minute_boundary();
        // The tick already recorded this minute's slot; ticking again a
        // few seconds later, still inside the same minute, finds nothing.
        let due = due_schedules(now + 30, vec![schedule(Some(now))]);
        assert!(
            due.is_empty(),
            "{:?}",
            due.iter().map(|d| d.slot).collect::<Vec<_>>()
        );
    }

    #[test]
    fn catch_up_runs_the_latest_missed_slot_only() {
        let now = minute_boundary();
        // The worker was down for five minutes; only one job starts, for
        // the latest slot, not one per minute missed.
        let due = due_schedules(now, vec![schedule(Some(now - 5 * 60))]);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].slot, now);
    }

    #[test]
    fn a_schedule_whose_slot_has_not_yet_come_is_not_due() {
        let now = minute_boundary();
        // Its last job already covers the latest slot at or before `now`.
        let due = due_schedules(now, vec![schedule(Some(now))]);
        assert!(due.is_empty());
    }

    #[test]
    fn every_five_minutes_only_matches_its_own_slots() {
        let now = minute_boundary() - (minute_boundary() % 300) + 300; // a "*/5" boundary
        let cron = Cron::from_str("*/5 * * * *").unwrap();
        let s = Schedule {
            project: "p".into(),
            workflow: "w".into(),
            cron,
            last_ref: None,
        };
        let due = due_schedules(now, vec![s]);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].slot, now);
        assert_eq!(due[0].slot % 300, 0);
    }
}
