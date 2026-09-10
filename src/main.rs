//! Forge 2: one binary, one command that matters.
//!
//! `forge run <repo> "<task>"` creates a worktree, runs the agent in it under
//! bubblewrap, re-runs the repository's declared checks, retries with the
//! check output as feedback, pushes on success, and records every attempt.
//! `forge add` queues the same thing and `forge work` drains the queue.

mod agent;
mod checks;
mod config;
mod git;
mod sandbox;
mod store;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use store::{Attempt, AttemptState, Store, Task, TaskState};

#[derive(Parser)]
#[command(name = "forge", about = "Forge 2")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args)]
struct TaskArgs {
    /// Path to a git repository containing a forge.toml
    repo: PathBuf,
    /// What to do, in plain language
    task: String,
    #[arg(long, default_value = "sonnet")]
    model: String,
    #[arg(long, default_value_t = 30)]
    max_turns: u32,
    /// Extra attempts after a failure, each fed the previous failure
    #[arg(long, default_value_t = 1)]
    retries: u32,
    /// Cost cap for this task in USD (default: per_task_usd in config.toml)
    #[arg(long)]
    budget: Option<f64>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run one task now: worktree → agent → declared checks → retry → push
    Run(TaskArgs),
    /// Queue a task for `forge work`
    Add(TaskArgs),
    /// Run queued tasks one at a time until the queue is empty
    Work {
        /// Stop after this many tasks
        #[arg(long)]
        max_tasks: Option<u32>,
    },
    /// List tasks, newest first
    Log {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Show one task and its attempts
    Show { id: i64 },
    /// Remove worktrees that are clean and whose commits are all on a remote
    Gc {
        /// Report what would happen without removing anything
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run(args) => run(args),
        Cmd::Add(args) => add(args),
        Cmd::Work { max_tasks } => work(max_tasks),
        Cmd::Log { limit } => Store::open(&paths()?.home.join("forge.db"))?.print_log(limit),
        Cmd::Show { id } => Store::open(&paths()?.home.join("forge.db"))?.print_show(id),
        Cmd::Gc { dry_run } => gc(dry_run),
    }
}

struct Paths {
    home: PathBuf,
    worktrees: PathBuf,
    logs: PathBuf,
}

/// Where Forge 2 keeps its database, worktrees, and logs. Separate from
/// Forge 1's FORGE_HOME so the two never touch each other's state.
fn paths() -> Result<Paths> {
    let home = if let Ok(p) = std::env::var("FORGE2_HOME") {
        PathBuf::from(p)
    } else if let Ok(p) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(p).join("forge2")
    } else {
        PathBuf::from(std::env::var("HOME").context("HOME is not set")?).join(".local/share/forge2")
    };
    let p = Paths {
        worktrees: home.join("worktrees"),
        logs: home.join("logs"),
        home,
    };
    std::fs::create_dir_all(&p.worktrees)?;
    std::fs::create_dir_all(&p.logs)?;
    Ok(p)
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A branch-safe slug from the first few words of the task text.
fn slug(task: &str) -> String {
    let mut out = String::new();
    for word in task.split_whitespace().take(5) {
        let w: String = word
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        if w.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&w);
    }
    out.chars()
        .take(32)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string()
}

/// Validate the request and insert a queued task. Shared by `run` and `add`.
fn enqueue(store: &Store, args: &TaskArgs) -> Result<Task> {
    let repo = args.repo.canonicalize().context("repo path")?;
    if !repo.join(".git").exists() {
        bail!("{} is not a git repository", repo.display());
    }
    let cfg = config::load(&repo)?;
    let mut t = Task {
        repo: repo.display().to_string(),
        task: args.task.clone(),
        base_branch: cfg.base_branch.clone(),
        model: args.model.clone(),
        max_turns: args.max_turns as i64,
        max_attempts: args.retries as i64 + 1,
        state: TaskState::Queued,
        created_at: unix_now(),
        budget_usd: args.budget,
        ..Default::default()
    };
    t.id = store.insert_task(&t)?;
    Ok(t)
}

fn run(args: TaskArgs) -> Result<()> {
    let p = paths()?;
    let store = Store::open(&p.home.join("forge.db"))?;
    if let Some(msg) = day_budget_reached(&store, &p)? {
        bail!("{msg}");
    }
    let t = enqueue(&store, &args)?;
    let claimed = store.claim_next(std::process::id() as i64)?;
    if claimed.as_ref().map(|c| c.id) != Some(t.id) {
        bail!(
            "task {} was queued but another worker claimed the queue head first; run `forge work`",
            t.id
        );
    }
    if drive(&store, &p, t.id) != TaskState::Succeeded {
        std::process::exit(1);
    }
    Ok(())
}

fn add(args: TaskArgs) -> Result<()> {
    let p = paths()?;
    let store = Store::open(&p.home.join("forge.db"))?;
    let t = enqueue(&store, &args)?;
    println!("queued task {} ({} queued)", t.id, store.queued_count()?);
    Ok(())
}

/// The rolling 24-hour cap. `Some(message)` when nothing more may start.
fn day_budget_reached(store: &Store, p: &Paths) -> Result<Option<String>> {
    let budget = config::load_budget(&p.home)?;
    let spent = store.spent_since(unix_now() - 86_400)?;
    Ok((spent >= budget.per_day_usd).then(|| {
        format!(
            "daily budget reached: ${spent:.2} of ${:.2} in the last 24h (per_day_usd in {})",
            budget.per_day_usd,
            p.home.join("config.toml").display()
        )
    }))
}

fn pid_alive(pid: i64) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Drain the queue one task at a time. A task that errors is recorded as
/// failed and the loop moves on; nothing here needs a human to diagnose.
fn work(max_tasks: Option<u32>) -> Result<()> {
    let p = paths()?;
    let store = Store::open(&p.home.join("forge.db"))?;
    for id in store.requeue_orphans(pid_alive)? {
        eprintln!("requeued task {id}: its previous worker exited");
    }
    let pid = std::process::id() as i64;
    let (mut done, mut ok) = (0u32, 0u32);
    while max_tasks.is_none_or(|m| done < m) {
        if let Some(msg) = day_budget_reached(&store, &p)? {
            eprintln!("{msg}; {} task(s) left queued", store.queued_count()?);
            break;
        }
        let Some(t) = store.claim_next(pid)? else {
            break;
        };
        eprintln!(
            "======== task {} ({} queued after this)",
            t.id,
            store.queued_count()?
        );
        if drive(&store, &p, t.id) == TaskState::Succeeded {
            ok += 1;
        }
        done += 1;
        eprintln!();
    }
    eprintln!("worked {done} task(s): {ok} succeeded, {} not", done - ok);
    Ok(())
}

/// Run a task to a terminal state, turning an internal error into a failed
/// task with the error as its reason.
fn drive(store: &Store, p: &Paths, id: i64) -> TaskState {
    match run_task(store, p, id) {
        Ok(state) => state,
        Err(e) => {
            eprintln!("ERROR    task {id}: {e:#}");
            if let Ok(Some(mut t)) = store.task(id) {
                t.state = TaskState::Failed;
                t.reason = format!("error: {e:#}");
                t.finished_at = Some(unix_now());
                t.worker_pid = None;
                let _ = store.update_task(&t);
            }
            TaskState::Failed
        }
    }
}

/// Nothing with unpublished work is deleted. A worktree goes only when it
/// is clean and every commit it added is reachable from a remote ref (or
/// it added none). Everything else is kept with the reason and the command
/// a human would run. Branches are never deleted.
fn gc(dry_run: bool) -> Result<()> {
    let p = paths()?;
    let store = Store::open(&p.home.join("forge.db"))?;
    let (mut removed, mut kept) = (0, 0);
    for t in store.tasks_with_worktrees()? {
        let wt = Path::new(&t.worktree);
        let repo = Path::new(&t.repo);
        let verdict: Result<Result<(), String>> = (|| {
            if !wt.exists() {
                git::worktree_prune(repo)?;
                return Ok(Ok(()));
            }
            if t.state == TaskState::Running {
                return Ok(Err("still running".into()));
            }
            if git::is_dirty(wt)? {
                return Ok(Err("uncommitted changes".into()));
            }
            let commits = git::count_commits(wt, &t.base_sha)?;
            if commits > 0 && !git::remote_contains_head(wt)? {
                return Ok(Err(format!("{commits} commit(s) not on any remote")));
            }
            if !dry_run {
                git::worktree_remove(repo, wt)?;
            }
            Ok(Ok(()))
        })();
        match verdict {
            Ok(Ok(())) => {
                removed += 1;
                if !dry_run {
                    store.mark_worktree_removed(t.id)?;
                }
                println!(
                    "task {:<4} {} {}",
                    t.id,
                    if dry_run {
                        "would remove"
                    } else {
                        "removed     "
                    },
                    t.worktree
                );
            }
            Ok(Err(reason)) => {
                kept += 1;
                println!("task {:<4} kept ({reason})", t.id);
                println!(
                    "           git -C {} worktree remove --force {}",
                    t.repo, t.worktree
                );
            }
            Err(e) => {
                kept += 1;
                println!("task {:<4} kept (error: {e:#})", t.id);
            }
        }
    }
    println!(
        "{} {removed}, kept {kept}",
        if dry_run { "would remove" } else { "removed" }
    );
    Ok(())
}

/// Drive one task to a terminal state: attempts until one succeeds or the
/// budget of attempts is spent. Each retry is told exactly what failed.
fn run_task(store: &Store, p: &Paths, id: i64) -> Result<TaskState> {
    let mut t = store.task(id)?.with_context(|| format!("no task {id}"))?;
    let repo = PathBuf::from(&t.repo);
    let cfg = config::load(&repo)?;
    let agent_bin = std::env::var("FORGE2_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
    let sandbox = sandbox::Sandbox::detect(&agent_bin)?;

    t.state = TaskState::Running;
    t.started_at = Some(unix_now());
    t.worker_pid = Some(std::process::id() as i64);
    if t.worktree.is_empty() {
        let base_name = format!("forge/{}-{}", t.id, slug(&t.task));
        t.branch = base_name.clone();
        for k in 2.. {
            if !git::branch_exists(&repo, &t.branch) {
                break;
            }
            t.branch = format!("{base_name}-{k}");
        }
        let wt = p.worktrees.join(t.id.to_string());
        git::worktree_add(&repo, &wt, &t.branch, &t.base_branch)?;
        t.base_sha = git::rev_parse(&wt, "HEAD")?;
        t.worktree = wt.display().to_string();
    }
    store.update_task(&t)?;
    let wt = PathBuf::from(&t.worktree);

    eprintln!("task     {}", t.id);
    eprintln!("worktree {}", wt.display());
    eprintln!(
        "branch   {} (from {} @ {})",
        t.branch,
        t.base_branch,
        &t.base_sha[..8]
    );
    eprintln!(
        "agent    {}, max {} turns, max {} attempts{}",
        t.model,
        t.max_turns,
        t.max_attempts,
        if sandbox.is_some() { ", sandboxed" } else { "" }
    );

    let task_cap = t
        .budget_usd
        .unwrap_or(config::load_budget(&p.home)?.per_task_usd);
    let prior = store.attempts(id)?.len() as i64;
    let mut feedback: Option<String> = None;
    let mut last = AttemptState::Running;
    let mut compare: Option<String> = None;
    let mut budget_stop: Option<String> = None;
    for n in (prior + 1)..=t.max_attempts {
        let spent = store.task_cost(id)?;
        if spent >= task_cap {
            budget_stop = Some(format!(
                "task budget reached: ${spent:.4} of ${task_cap:.2} after {} attempt(s)",
                n - 1
            ));
            break;
        }
        eprintln!("--- attempt {n} of {}", t.max_attempts);
        let (a, results) =
            run_attempt(store, p, &t, &cfg, sandbox.as_ref(), n, feedback.as_deref())?;
        last = a.state;
        match a.state {
            AttemptState::Succeeded => {
                if let Some(remote) = &cfg.push_remote {
                    match git::push(&wt, remote, &t.branch) {
                        Ok(()) => {
                            t.pushed = true;
                            compare = git::remote_url(&repo, remote)
                                .and_then(|u| git::compare_url(&u, &t.base_branch, &t.branch));
                            eprintln!("pushed   {remote}/{}", t.branch);
                        }
                        Err(e) => eprintln!("push     FAILED: {e:#}"),
                    }
                } else {
                    eprintln!("push     skipped (no remote configured)");
                }
                break;
            }
            AttemptState::Unverified => break,
            AttemptState::ChecksFailed => {
                let mut fb = String::from("The previous attempt's declared checks failed:\n");
                for r in results.iter().filter(|r| !r.ok) {
                    fb.push_str(&format!(
                        "- {} (exit {}):\n",
                        r.name,
                        r.exit.map_or("signal".into(), |c| c.to_string())
                    ));
                    for l in r.tail.lines() {
                        fb.push_str(&format!("    {l}\n"));
                    }
                }
                fb.push_str("Fix the failures and commit.");
                feedback = Some(fb);
            }
            AttemptState::AgentFailed => {
                feedback = Some(format!(
                    "The previous attempt ended without a result (agent exit {}, {} turns of {} allowed). \
                     Continue from the current state of the branch, be economical with turns, and commit as soon as the task is done.",
                    a.agent_exit.map_or("signal".into(), |c| c.to_string()),
                    a.num_turns,
                    t.max_turns
                ));
            }
            AttemptState::Running => unreachable!("attempt returned in running state"),
        }
    }

    let attempts = store.attempts(id)?;
    let cost = store.task_cost(id)?;
    t.state = match last {
        AttemptState::Succeeded => TaskState::Succeeded,
        AttemptState::Unverified => TaskState::Unverified,
        _ => TaskState::Failed,
    };
    t.reason = if let Some(b) = budget_stop {
        b
    } else {
        match last {
            AttemptState::Succeeded => String::new(),
            AttemptState::Unverified => "no checks declared; branch not pushed".into(),
            AttemptState::ChecksFailed => {
                format!("checks failed after {} attempt(s)", attempts.len())
            }
            AttemptState::AgentFailed => {
                format!("agent failed after {} attempt(s)", attempts.len())
            }
            AttemptState::Running => "no attempts ran".into(),
        }
    };
    t.finished_at = Some(unix_now());
    t.worker_pid = None;
    store.update_task(&t)?;

    eprintln!();
    eprintln!(
        "{}  task {} ({} attempt(s), ${cost:.4}){}",
        t.state.as_str().to_uppercase(),
        t.id,
        attempts.len(),
        if t.reason.is_empty() {
            String::new()
        } else {
            format!(": {}", t.reason)
        }
    );
    eprintln!(
        "  branch   {}{}",
        t.branch,
        if t.pushed { " (pushed)" } else { "" }
    );
    if let Some(u) = compare {
        eprintln!("  compare  {u}");
    }
    eprintln!(
        "  remove   git -C {} worktree remove {}",
        repo.display(),
        wt.display()
    );
    Ok(t.state)
}

fn run_attempt(
    store: &Store,
    p: &Paths,
    t: &Task,
    cfg: &config::Config,
    sandbox: Option<&sandbox::Sandbox>,
    n: i64,
    feedback: Option<&str>,
) -> Result<(Attempt, Vec<checks::CheckResult>)> {
    let wt = Path::new(&t.worktree);
    let repo_git_dir = Path::new(&t.repo).join(".git");
    let check_names: Vec<&str> = cfg.checks.keys().map(String::as_str).collect();
    let mut prompt = format!(
        "You are working in a git worktree on branch `{branch}` (based on `{base}`). \
         Complete the task below, then commit your work with a clear message. Do not push. \
         After you finish, these declared checks are re-run by the operator: {checks}. \
         Anything you report is a claim; only those checks decide.\n\nTask:\n{task}",
        branch = t.branch,
        base = t.base_branch,
        checks = if check_names.is_empty() {
            "(none)".to_string()
        } else {
            check_names.join(", ")
        },
        task = t.task,
    );
    if let Some(fb) = feedback {
        prompt.push_str(&format!(
            "\n\nThis is attempt {n} of {}. Your earlier commits are already on this branch.\n{fb}",
            t.max_attempts
        ));
    }

    let log_path = p.logs.join(format!("{}-{n}.jsonl", t.id));
    let mut a = Attempt {
        task_id: t.id,
        attempt_no: n,
        state: AttemptState::Running,
        started_at: unix_now(),
        log_path: log_path.display().to_string(),
        ..Default::default()
    };
    a.id = store.insert_attempt(&a)?;

    let outcome = agent::run(agent::Launch {
        worktree: wt,
        repo_git_dir: &repo_git_dir,
        prompt: &prompt,
        model: &t.model,
        max_turns: t.max_turns as u32,
        log_path: &log_path,
        sandbox,
    })?;
    eprintln!(
        "agent    exit {} · {} turns · {} tool calls · {:.1}s · {}",
        outcome
            .exit_code
            .map_or("signal".to_string(), |c| c.to_string()),
        outcome.num_turns,
        outcome.tool_calls,
        outcome.wall_ms as f64 / 1000.0,
        outcome
            .cost_usd
            .map_or("cost n/a".to_string(), |c| format!("${c:.4}")),
    );

    a.commits = git::count_commits(wt, &t.base_sha)?;
    a.files_changed = git::files_changed(wt, &t.base_sha)?;
    a.dirty = git::is_dirty(wt)?;
    eprintln!(
        "git      {} commit(s), {} file(s) changed{}",
        a.commits,
        a.files_changed,
        if a.dirty {
            ", UNCOMMITTED changes left behind"
        } else {
            ""
        }
    );

    let results = checks::run_all(wt, &repo_git_dir, &cfg.checks, sandbox);

    let agent_failed = outcome.is_error || !outcome.got_result || outcome.exit_code != Some(0);
    a.state = if agent_failed {
        AttemptState::AgentFailed
    } else if cfg.checks.is_empty() {
        AttemptState::Unverified
    } else if results.iter().any(|r| !r.ok) {
        AttemptState::ChecksFailed
    } else {
        AttemptState::Succeeded
    };
    a.finished_at = Some(unix_now());
    a.agent_exit = outcome.exit_code;
    a.num_turns = outcome.num_turns;
    a.tool_calls = outcome.tool_calls;
    a.cost_usd = outcome.cost_usd;
    a.agent_ms = outcome.wall_ms as i64;
    a.checks_json = serde_json::to_string(&results)?;
    a.result_text = outcome.result_text;
    store.finish_attempt(&a)?;
    eprintln!("attempt  {}", a.state.as_str());
    Ok((a, results))
}
