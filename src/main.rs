//! Forge 2 proof of concept: one binary, one command that matters.
//!
//! `forge run <repo> "<task>"` creates a worktree, runs the agent in it,
//! re-runs the repository's declared checks, writes one fact row, and
//! prints the result. Nothing else exists yet.

mod agent;
mod checks;
mod config;
mod git;
mod sandbox;
mod store;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(name = "forge", about = "Forge 2 proof of concept")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run one task: worktree → agent → declared checks → fact row
    Run {
        /// Path to a git repository containing a forge.toml
        repo: PathBuf,
        /// What to do, in plain language
        task: String,
        #[arg(long, default_value = "sonnet")]
        model: String,
        #[arg(long, default_value_t = 30)]
        max_turns: u32,
    },
    /// List recorded attempts, newest first
    Log {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Show one attempt in full
    Show { id: String },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run {
            repo,
            task,
            model,
            max_turns,
        } => run(repo, task, model, max_turns),
        Cmd::Log { limit } => store::Store::open(&db_path()?)?.print_log(limit),
        Cmd::Show { id } => store::Store::open(&db_path()?)?.print_show(&id),
    }
}

/// Where Forge 2 keeps its database, worktrees, and logs. Separate from
/// Forge 1's FORGE_HOME so the two never touch each other's state.
fn forge_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("FORGE2_HOME") {
        return Ok(PathBuf::from(p));
    }
    if let Ok(p) = std::env::var("XDG_DATA_HOME") {
        return Ok(PathBuf::from(p).join("forge2"));
    }
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/share/forge2"))
}

fn db_path() -> Result<PathBuf> {
    let home = forge_home()?;
    std::fs::create_dir_all(&home)?;
    Ok(home.join("forge.db"))
}

fn new_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}-{:04x}", now.as_secs(), now.subsec_nanos() as u16)
}

fn run(repo: PathBuf, task: String, model: String, max_turns: u32) -> Result<()> {
    let repo = repo.canonicalize().context("repo path")?;
    if !repo.join(".git").exists() {
        bail!("{} is not a git repository", repo.display());
    }
    let cfg = config::load(&repo)?;
    let agent_bin = std::env::var("FORGE2_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
    let sandbox = sandbox::Sandbox::detect(&agent_bin)?;
    let home = forge_home()?;
    let worktrees = home.join("worktrees");
    let logs = home.join("logs");
    std::fs::create_dir_all(&worktrees)?;
    std::fs::create_dir_all(&logs)?;
    let store = store::Store::open(&home.join("forge.db"))?;

    let id = new_id();
    let branch = format!("forge/{id}");
    let wt = worktrees.join(&id);
    git::worktree_add(&repo, &wt, &branch, &cfg.base_branch)?;
    let base_sha = git::rev_parse(&wt, "HEAD")?;

    let mut attempt = store::Attempt {
        id: id.clone(),
        repo: repo.display().to_string(),
        task: task.clone(),
        branch: branch.clone(),
        worktree: wt.display().to_string(),
        base_sha: base_sha.clone(),
        model: model.clone(),
        state: store::State::Running,
        started_at: unix_now(),
        ..Default::default()
    };
    store.insert(&attempt)?;

    eprintln!("attempt  {id}");
    eprintln!("worktree {}", wt.display());
    eprintln!(
        "branch   {branch} (from {} @ {})",
        cfg.base_branch,
        &base_sha[..8]
    );
    eprintln!(
        "agent    {model}, max {max_turns} turns{}",
        if sandbox.is_some() { ", sandboxed" } else { "" }
    );

    let check_names: Vec<&str> = cfg.checks.keys().map(String::as_str).collect();
    let prompt = format!(
        "You are working in a git worktree on branch `{branch}` (based on `{base}`). \
         Complete the task below, then commit your work with a clear message. Do not push. \
         After you finish, these declared checks are re-run by the operator: {checks}. \
         Anything you report is a claim; only those checks decide.\n\nTask:\n{task}",
        base = cfg.base_branch,
        checks = if check_names.is_empty() {
            "(none)".to_string()
        } else {
            check_names.join(", ")
        },
    );

    let log_path = logs.join(format!("{id}.jsonl"));
    let outcome = agent::run(agent::Launch {
        worktree: &wt,
        repo_git_dir: &repo.join(".git"),
        prompt: &prompt,
        model: &model,
        max_turns,
        log_path: &log_path,
        sandbox: sandbox.as_ref(),
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

    let commits = git::count_commits(&wt, &base_sha)?;
    let files = git::files_changed(&wt, &base_sha)?;
    let dirty = git::is_dirty(&wt)?;
    eprintln!(
        "git      {commits} commit(s), {files} file(s) changed{}",
        if dirty {
            ", UNCOMMITTED changes left behind"
        } else {
            ""
        }
    );

    let results = checks::run_all(&wt, &cfg.checks);

    let agent_failed = outcome.is_error || !outcome.got_result || outcome.exit_code != Some(0);
    attempt.state = if agent_failed {
        store::State::AgentFailed
    } else if cfg.checks.is_empty() {
        store::State::Unverified
    } else if results.iter().any(|r| !r.ok) {
        store::State::ChecksFailed
    } else {
        store::State::Succeeded
    };
    attempt.finished_at = Some(unix_now());
    attempt.agent_exit = outcome.exit_code;
    attempt.num_turns = outcome.num_turns;
    attempt.tool_calls = outcome.tool_calls;
    attempt.cost_usd = outcome.cost_usd;
    attempt.agent_ms = outcome.wall_ms as i64;
    attempt.commits = commits;
    attempt.files_changed = files;
    attempt.dirty = dirty;
    attempt.checks_json = serde_json::to_string(&results)?;
    attempt.result_text = outcome.result_text;

    let mut compare: Option<String> = None;
    if attempt.state == store::State::Succeeded {
        if let Some(remote) = &cfg.push_remote {
            match git::push(&wt, remote, &branch) {
                Ok(()) => {
                    attempt.pushed = true;
                    compare = git::remote_url(&repo, remote)
                        .and_then(|u| git::compare_url(&u, &cfg.base_branch, &branch));
                    eprintln!("pushed   {remote}/{branch}");
                }
                Err(e) => eprintln!("push     FAILED: {e:#}"),
            }
        } else {
            eprintln!("push     skipped (no remote configured)");
        }
    }
    store.finish(&attempt)?;

    eprintln!();
    eprintln!("{}  {id}", attempt.state.as_str().to_uppercase());
    eprintln!("  branch   {branch}");
    if let Some(u) = compare {
        eprintln!("  compare  {u}");
    }
    eprintln!("  log      {}", log_path.display());
    eprintln!(
        "  remove   git -C {} worktree remove {}",
        repo.display(),
        wt.display()
    );
    if attempt.state != store::State::Succeeded {
        std::process::exit(1);
    }
    Ok(())
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
