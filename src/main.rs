//! Forge 2: one binary, one command that matters.
//!
//! `forge run <repo> "<task>"` creates a worktree, runs the agent in it under
//! bubblewrap, re-runs the repository's declared checks, retries with the
//! check output as feedback, pushes on success, and records every attempt.

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

#[derive(Subcommand)]
enum Cmd {
    /// Run one task now: worktree → agent → declared checks → retry → push
    Run {
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
    },
    /// List tasks, newest first
    Log {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Show one task and its attempts
    Show { id: i64 },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run {
            repo,
            task,
            model,
            max_turns,
            retries,
        } => run(repo, task, model, max_turns, retries),
        Cmd::Log { limit } => Store::open(&paths()?.home.join("forge.db"))?.print_log(limit),
        Cmd::Show { id } => Store::open(&paths()?.home.join("forge.db"))?.print_show(id),
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

fn unix_now() -> i64 {
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

fn run(repo: PathBuf, task: String, model: String, max_turns: u32, retries: u32) -> Result<()> {
    let repo = repo.canonicalize().context("repo path")?;
    if !repo.join(".git").exists() {
        bail!("{} is not a git repository", repo.display());
    }
    let cfg = config::load(&repo)?;
    let p = paths()?;
    let store = Store::open(&p.home.join("forge.db"))?;
    let t = Task {
        repo: repo.display().to_string(),
        task,
        base_branch: cfg.base_branch.clone(),
        model,
        max_turns: max_turns as i64,
        max_attempts: retries as i64 + 1,
        state: TaskState::Queued,
        created_at: unix_now(),
        ..Default::default()
    };
    let id = store.insert_task(&t)?;
    let state = run_task(&store, &p, id)?;
    if state != TaskState::Succeeded {
        std::process::exit(1);
    }
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
    if t.worktree.is_empty() {
        t.branch = format!("forge/{}-{}", t.id, slug(&t.task));
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

    let prior = store.attempts(id)?.len() as i64;
    let mut feedback: Option<String> = None;
    let mut last = AttemptState::Running;
    let mut compare: Option<String> = None;
    for n in (prior + 1)..=t.max_attempts {
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
    t.reason = match last {
        AttemptState::Succeeded => String::new(),
        AttemptState::Unverified => "no checks declared; branch not pushed".into(),
        AttemptState::ChecksFailed => format!("checks failed after {} attempt(s)", attempts.len()),
        AttemptState::AgentFailed => format!("agent failed after {} attempt(s)", attempts.len()),
        AttemptState::Running => "no attempts ran".into(),
    };
    t.finished_at = Some(unix_now());
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
