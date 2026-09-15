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
use crate::report::Event;
use crate::store::TaskState;
use crate::unix_now;
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;

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
            // The rung before the human: the supervisor reads the record
            // and answers, files a prerequisite, or escalates. Its own
            // failure is a note, never a task failure.
            if let Err(e) = crate::supervisor::supervise(&f, id).await {
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

/// A subscription window at or over its cap, by the latest sample any
/// attempt recorded: the message and the unix second the hold ends. A
/// window whose reset time has passed no longer holds anything.
pub fn window_hold(f: &Forge) -> Result<Option<(String, i64)>> {
    let Some(s) = f.store.latest_rate_limit()? else {
        return Ok(None);
    };
    let now = unix_now();
    let mut hold: Option<(String, i64)> = None;
    for (name, util, resets, cap) in [
        (
            "5h",
            s.five_hour,
            s.five_hour_resets,
            f.budget.five_hour_max,
        ),
        (
            "7d",
            s.seven_day,
            s.seven_day_resets,
            f.budget.seven_day_max,
        ),
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

fn p_config(f: &Forge) -> String {
    f.paths.home.join("config.toml").display().to_string()
}

pub struct WorkOpts {
    pub jobs: usize,
    /// Seconds between queue polls when idle; `None` exits when idle.
    pub poll: Option<u64>,
    pub max_tasks: Option<u32>,
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

pub async fn work(f: Arc<Forge>, opts: WorkOpts) -> Result<()> {
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
    let mut running: JoinSet<(i64, Result<TaskState>)> = JoinSet::new();
    let mut ids: Vec<i64> = Vec::new();
    let (mut done, mut ok) = (0u32, 0u32);
    let mut stopping = false;
    let mut env_error: Option<anyhow::Error> = None;
    let mut claimed = 0u32;
    let mut hold_until: Option<i64> = None;

    loop {
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
            for (t, d, why) in f.store.block_dependents()? {
                eprintln!("task {t} blocked: {why} (task {d})");
            }
            if let Some((msg, until)) = window_hold(&f)? {
                if f.store.queued_count()? > 0 && hold_until != Some(until) {
                    eprintln!("{msg}; holding, {} task(s) queued", f.store.queued_count()?);
                }
                hold_until = Some(until);
                break;
            }
            hold_until = None;
            let Some(t) = f.store.claim_next(pid)? else {
                break;
            };
            claimed += 1;
            eprintln!(
                "======== task {} starting ({} queued, {} running)",
                t.id,
                f.store.queued_count()?,
                running.len() + 1
            );
            ids.push(t.id);
            let fc = f.clone();
            running.spawn(async move { (t.id, drive(fc, t.id).await) });
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
                        _ = shutdown_signal() => { eprintln!("stopping"); break }
                    }
                }
                (None, Some(secs)) if !stopping && env_error.is_none() && !exhausted => {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(secs)) => continue,
                        _ = shutdown_signal() => { eprintln!("stopping"); break }
                    }
                }
                _ => break,
            }
        }

        tokio::select! {
            Some(joined) = running.join_next() => {
                match joined {
                    Ok((id, Ok(state))) => {
                        ids.retain(|&x| x != id);
                        done += 1;
                        if state == TaskState::Succeeded { ok += 1 }
                        eprintln!("======== task {id} {}\n", state.as_str());
                    }
                    Ok((id, Err(e))) => {
                        ids.retain(|&x| x != id);
                        eprintln!("======== task {id} could not run: {e:#}");
                        env_error = Some(e);
                        stopping = true;
                    }
                    Err(join) => {
                        eprintln!("a task panicked: {join}");
                        stopping = true;
                    }
                }
            }
            _ = shutdown_signal() => {
                if !stopping {
                    stopping = true;
                    eprintln!("stopping: no new tasks; {} running attempt(s) will finish (signal again to abort them)", running.len());
                } else {
                    eprintln!("aborting {} running attempt(s) and requeueing their tasks", running.len());
                    running.abort_all();
                    while running.join_next().await.is_some() {}
                    for id in ids.drain(..) {
                        f.store.requeue(id, "worker aborted by operator")?;
                    }
                    break;
                }
            }
        }
    }

    plugins.stop().await;
    eprintln!("worked {done} task(s): {ok} succeeded, {} not", done - ok);
    match env_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
