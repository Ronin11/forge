//! Commands and their terminal output. The engine never prints; this does.

use crate::ctx::Forge;
use crate::store::{Task, TaskState};
use crate::{config, git, unix_now, worker};
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "forge", about = "Forge 2")]
pub struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args)]
pub struct TaskArgs {
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
    /// Wall-clock limit per attempt; the agent is killed past it
    #[arg(long, default_value_t = 1800)]
    timeout_secs: u32,
    /// Cost cap for this task in USD (default: per_task_usd in config.toml)
    #[arg(long)]
    budget: Option<f64>,
    /// A shell command that must exit 0 in the worktree for the task to be
    /// done (repeatable). Run after the repo's own checks.
    #[arg(long = "check")]
    checks: Vec<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run one task now
    Run(TaskArgs),
    /// Queue a task for `forge work`
    Add(TaskArgs),
    /// Run queued tasks until the queue is empty
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

pub async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run(args) => run(args).await,
        Cmd::Add(args) => add(args).await,
        Cmd::Work { max_tasks } => {
            worker::work(Arc::new(Forge::open(true, false)?), max_tasks).await
        }
        Cmd::Log { limit } => log(limit),
        Cmd::Show { id } => show(id),
        Cmd::Gc { dry_run } => gc(dry_run).await,
    }
}

/// Validate the request and insert a queued task. Refuses a task nothing
/// would verify: a repo with no `[checks]` and a task with no `--check`.
async fn enqueue(f: &Forge, args: &TaskArgs) -> Result<Task> {
    let repo = args.repo.canonicalize().context("repo path")?;
    if !repo.join(".git").exists() {
        bail!("{} is not a git repository", repo.display());
    }
    let cfg = config::load_working(&repo).await?;
    if cfg.checks.is_empty() && args.checks.is_empty() {
        bail!(
            "{} declares no [checks] and the task declares no --check; nothing would verify the work",
            repo.join("forge.toml").display()
        );
    }
    let mut t = Task {
        repo: repo.display().to_string(),
        task: args.task.clone(),
        base_branch: cfg.base_branch.clone(),
        model: args.model.clone(),
        max_turns: args.max_turns as i64,
        max_attempts: args.retries as i64 + 1,
        timeout_secs: args.timeout_secs as i64,
        checks: args.checks.clone(),
        state: TaskState::Queued,
        created_at: unix_now(),
        budget_usd: args.budget,
        ..Default::default()
    };
    t.id = f.store.insert_task(&t)?;
    Ok(t)
}

async fn run(args: TaskArgs) -> Result<()> {
    let f = Arc::new(Forge::open(true, false)?);
    if let Some(msg) = worker::day_budget_reached(&f)? {
        bail!("{msg}");
    }
    let t = enqueue(&f, &args).await?;
    if !f.store.claim(t.id, std::process::id() as i64)? {
        bail!(
            "task {} was claimed by another worker before this one could start it",
            t.id
        );
    }
    eprintln!("task     {}", t.id);
    if worker::drive(f, t.id).await? != TaskState::Succeeded {
        std::process::exit(1);
    }
    Ok(())
}

async fn add(args: TaskArgs) -> Result<()> {
    let f = Forge::open(false, false)?;
    let t = enqueue(&f, &args).await?;
    println!("queued task {} ({} queued)", t.id, f.store.queued_count()?);
    Ok(())
}

fn log(limit: u32) -> Result<()> {
    let f = Forge::open(false, false)?;
    println!(
        "{:<5} {:<11} {:<3} {:<8} {:<19} {:<18} TASK",
        "ID", "STATE", "ATT", "COST", "CREATED", "REPO"
    );
    for s in f.store.list_tasks(limit)? {
        let repo_name = Path::new(&s.repo)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(s.repo);
        let task_short: String = s
            .task
            .chars()
            .take(50)
            .collect::<String>()
            .replace('\n', " ");
        println!(
            "{:<5} {:<11} {:<3} {:<8} {:<19} {:<18} {}",
            s.id,
            s.state,
            s.attempts,
            format!("${:.4}", s.cost),
            s.created,
            repo_name,
            task_short
        );
    }
    Ok(())
}

fn show(id: i64) -> Result<()> {
    let f = Forge::open(false, false)?;
    let Some(t) = f.store.task(id)? else {
        bail!("no task {id}")
    };
    let attempts = f.store.attempts(id)?;
    let cost: f64 = attempts.iter().filter_map(|a| a.cost_usd).sum();
    println!("task       {}", t.id);
    println!(
        "state      {}{}",
        t.state.as_str(),
        if t.reason.is_empty() {
            String::new()
        } else {
            format!(" ({})", t.reason)
        }
    );
    println!("repo       {}", t.repo);
    println!(
        "base       {} @ {}",
        t.base_branch,
        if t.base_sha.is_empty() {
            "-"
        } else {
            &t.base_sha[..8]
        }
    );
    println!(
        "branch     {}{}",
        if t.branch.is_empty() { "-" } else { &t.branch },
        if t.pushed { " (pushed)" } else { "" }
    );
    println!(
        "worktree   {}{}",
        if t.worktree.is_empty() {
            "-"
        } else {
            &t.worktree
        },
        if t.worktree_removed_at.is_some() {
            " (removed)"
        } else {
            ""
        }
    );
    println!(
        "model      {} (max {} turns, max {} attempts, {}s timeout)",
        t.model, t.max_turns, t.max_attempts, t.timeout_secs
    );
    println!(
        "cost       ${cost:.4} over {} attempt(s){}",
        attempts.len(),
        t.budget_usd
            .map_or(String::new(), |b| format!(" (task cap ${b:.2})"))
    );
    for c in &t.checks {
        println!("check      $ {c}");
    }
    println!("text       {}", t.task);
    for a in &attempts {
        println!();
        println!(
            "attempt {}  {}{}  {}  {} turns  {} tools  {:.1}s  {}  {} commit(s)  {} file(s){}",
            a.attempt_no,
            a.state.as_str(),
            if a.reason.is_empty() {
                String::new()
            } else {
                format!(" ({})", a.reason)
            },
            if a.timed_out {
                "TIMED OUT".to_string()
            } else {
                format!(
                    "exit {}",
                    a.agent_exit.map_or("-".into(), |v| v.to_string())
                )
            },
            a.num_turns,
            a.tool_calls,
            a.agent_ms as f64 / 1000.0,
            a.cost_usd.map_or("-".into(), |c| format!("${c:.4}")),
            a.commits,
            a.files_changed,
            if a.dirty { "  DIRTY" } else { "" }
        );
        println!("  log     {}", a.log_path);
        if let Ok(checks) = serde_json::from_str::<Vec<crate::checks::CheckResult>>(&a.verdict_json)
        {
            for c in checks {
                println!(
                    "  {} {} {} ({:.1}s)",
                    if c.ok { "✓" } else { "✗" },
                    c.level,
                    c.name,
                    c.ms as f64 / 1000.0
                );
            }
        }
        if !a.result_text.is_empty() {
            let first: String = a
                .result_text
                .lines()
                .take(3)
                .collect::<Vec<_>>()
                .join(" / ");
            println!("  result  {}", first.chars().take(200).collect::<String>());
        }
    }
    Ok(())
}

/// Nothing with unpublished work is deleted. A worktree goes only when it
/// is clean and every commit it added is reachable from a remote ref (or
/// it added none). Everything else is kept with the reason and the command
/// a human would run. Branches are never deleted.
async fn gc(dry_run: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let (mut removed, mut kept) = (0, 0);
    for t in f.store.tasks_with_worktrees()? {
        let wt = Path::new(&t.worktree);
        let repo = Path::new(&t.repo);
        let verdict: Result<Result<(), String>> = async {
            if !wt.exists() {
                git::worktree_prune(repo).await?;
                return Ok(Ok(()));
            }
            if t.state == TaskState::Running {
                return Ok(Err("still running".into()));
            }
            if !git::dirty_paths(wt).await?.is_empty() {
                return Ok(Err("uncommitted changes".into()));
            }
            let commits = git::count_commits(wt, &t.base_sha).await?;
            if commits > 0 && !git::remote_contains_head(wt).await? {
                return Ok(Err(format!("{commits} commit(s) not on any remote")));
            }
            if !dry_run {
                git::worktree_remove(repo, wt).await?;
            }
            Ok(Ok(()))
        }
        .await;
        match verdict {
            Ok(Ok(())) => {
                removed += 1;
                if !dry_run {
                    f.store.mark_worktree_removed(t.id)?;
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
