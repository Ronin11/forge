//! Drive one task to a terminal state: attempts until one is verified or
//! the attempt or cost budget is spent, each retry told exactly what
//! failed. Every error is classified: a `Task` fault is this task's
//! problem and it fails; an `Env` fault means the worker itself cannot do
//! its job and must stop without blaming the task.

use crate::ctx::Forge;
use crate::report::Event;
use crate::store::{Attempt, AttemptState, Task, TaskState};
use crate::verify::{self, Subject, Verdict};
use crate::{agent, config, git, unix_now};
use anyhow::Context;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub enum Fault {
    Task(anyhow::Error),
    Env(anyhow::Error),
}

pub trait Classify<T> {
    fn task(self) -> Result<T, Fault>;
    fn env(self) -> Result<T, Fault>;
}

impl<T, E: Into<anyhow::Error>> Classify<T> for Result<T, E> {
    fn task(self) -> Result<T, Fault> {
        self.map_err(|e| Fault::Task(e.into()))
    }
    fn env(self) -> Result<T, Fault> {
        self.map_err(|e| Fault::Env(e.into()))
    }
}

/// A branch-safe slug from the first few words of the task text.
pub fn slug(task: &str) -> String {
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

pub async fn run_task(f: Arc<Forge>, id: i64) -> Result<TaskState, Fault> {
    let mut t = f
        .store
        .task(id)
        .env()?
        .with_context(|| format!("no task {id}"))
        .env()?;
    let repo = PathBuf::from(&t.repo);

    t.state = TaskState::Running;
    t.started_at = Some(unix_now());
    t.worker_pid = Some(std::process::id() as i64);
    if t.worktree.is_empty() {
        let base_name = format!("forge/{}-{}", t.id, slug(&t.task));
        t.branch = base_name.clone();
        for k in 2.. {
            if !git::branch_exists(&repo, &t.branch).await {
                break;
            }
            t.branch = format!("{base_name}-{k}");
        }
        let wt = f.paths.worktrees.join(t.id.to_string());
        git::worktree_add(&repo, &wt, &t.branch, &t.base_branch)
            .await
            .task()?;
        t.base_sha = git::rev_parse(&wt, "HEAD").await.task()?;
        t.worktree = wt.display().to_string();
    }
    f.store.update_task(&t).env()?;
    let wt = PathBuf::from(&t.worktree);
    // Checks come from the trusted base, never from the branch under test.
    let cfg = config::load_at(&repo, &t.base_sha).await.task()?;

    f.report.emit(
        id,
        Event::TaskStarted {
            worktree: &t.worktree,
            branch: &t.branch,
            base_branch: &t.base_branch,
            base_sha: &t.base_sha,
            model: &t.model,
            max_turns: t.max_turns,
            max_attempts: t.max_attempts,
            timeout_secs: t.timeout_secs,
            sandboxed: f.sandboxed(),
        },
    );

    let task_cap = t.budget_usd.unwrap_or(f.budget.per_task_usd);
    let prior = f.store.attempts(id).env()?.len() as i64;
    let mut feedback: Option<String> = None;
    let mut last = AttemptState::Running;
    let mut last_reason = String::new();
    let mut budget_stop: Option<String> = None;
    let mut compare: Option<String> = None;
    for n in (prior + 1)..=t.max_attempts {
        let spent = f.store.task_cost(id).env()?;
        if spent >= task_cap {
            budget_stop = Some(format!(
                "task budget reached: ${spent:.4} of ${task_cap:.2} after {} attempt(s)",
                n - 1
            ));
            break;
        }
        f.report.emit(
            id,
            Event::AttemptStarted {
                n,
                of: t.max_attempts,
            },
        );
        let (a, verdict, outcome) = run_attempt(&f, &t, &cfg, n, feedback.as_deref()).await?;
        last = a.state;
        last_reason = a.reason.clone();
        match a.state {
            AttemptState::Succeeded => {
                if let Some(remote) = &cfg.push_remote {
                    match git::push(&wt, remote, &t.branch).await {
                        Ok(()) => {
                            t.pushed = true;
                            compare = git::remote_url(&repo, remote)
                                .await
                                .and_then(|u| git::compare_url(&u, &t.base_branch, &t.branch));
                            f.report.emit(
                                id,
                                Event::Pushed {
                                    remote,
                                    branch: &t.branch,
                                },
                            );
                        }
                        Err(e) => {
                            last_reason = format!("push failed: {e:#}");
                            f.report.emit(
                                id,
                                Event::PushFailed {
                                    error: &format!("{e:#}"),
                                },
                            );
                        }
                    }
                } else {
                    f.report.emit(id, Event::PushSkipped);
                }
                break;
            }
            AttemptState::Unverified => break,
            AttemptState::ChecksFailed | AttemptState::AgentFailed => {
                feedback = Some(verify::feedback(&verdict, &outcome, t.max_turns));
            }
            AttemptState::Running => unreachable!("attempt returned in running state"),
        }
    }

    let attempts = f.store.attempts(id).env()?;
    let cost = f.store.task_cost(id).env()?;
    t.state = match last {
        AttemptState::Succeeded => TaskState::Succeeded,
        AttemptState::Unverified => TaskState::Unverified,
        _ => TaskState::Failed,
    };
    t.reason = match (budget_stop, last) {
        (Some(b), _) => b,
        (None, AttemptState::Succeeded) => last_reason,
        (None, AttemptState::Running) => "no attempts ran".into(),
        (None, _) => format!("{last_reason} (after {} attempt(s))", attempts.len()),
    };
    t.finished_at = Some(unix_now());
    t.worker_pid = None;
    f.store.update_task(&t).env()?;

    f.report.emit(
        id,
        Event::TaskDone {
            state: t.state.as_str(),
            attempts: attempts.len(),
            cost,
            reason: &t.reason,
            branch: &t.branch,
            pushed: t.pushed,
            compare: compare.as_deref(),
            remove_cmd: &format!("git -C {} worktree remove {}", repo.display(), wt.display()),
        },
    );
    Ok(t.state)
}

fn prompt(t: &Task, cfg: &config::Config, n: i64, feedback: Option<&str>) -> String {
    let l1: Vec<&str> = cfg.checks.keys().map(String::as_str).collect();
    let mut p = format!(
        "You are working in a git worktree on branch `{branch}` (based on `{base}`). \
         Complete the task below, then commit your work with a clear message. Do not push. \
         Leave the tree clean: every change committed, nothing untracked. Do not modify forge.toml.\n\n\
         After you finish, the operator re-runs the repository's declared checks: {l1}.",
        branch = t.branch,
        base = t.base_branch,
        l1 = if l1.is_empty() {
            "(none)".to_string()
        } else {
            l1.join(", ")
        },
    );
    if !t.checks.is_empty() {
        p.push_str("\nThe task is only done when these commands also exit 0 in the worktree:\n");
        for c in &t.checks {
            p.push_str(&format!("  $ {c}\n"));
        }
    }
    p.push_str(&format!(
        "Anything you report is a claim; only those checks decide.\n\nTask:\n{}",
        t.task
    ));
    if let Some(fb) = feedback {
        p.push_str(&format!(
            "\n\nThis is attempt {n} of {}. Your earlier commits are already on this branch.\n{fb}",
            t.max_attempts
        ));
    }
    p
}

async fn run_attempt(
    f: &Forge,
    t: &Task,
    cfg: &config::Config,
    n: i64,
    feedback: Option<&str>,
) -> Result<(Attempt, Verdict, agent::Outcome), Fault> {
    let wt = Path::new(&t.worktree);
    let repo_git_dir = Path::new(&t.repo).join(".git");
    let log_path = f.paths.logs.join(format!("{}-{n}.jsonl", t.id));
    let mut a = Attempt {
        task_id: t.id,
        attempt_no: n,
        state: AttemptState::Running,
        started_at: unix_now(),
        log_path: log_path.display().to_string(),
        ..Default::default()
    };
    a.id = f.store.insert_attempt(&a).env()?;

    let outcome = agent::run(agent::Launch {
        task_id: t.id,
        worktree: wt,
        repo_git_dir: &repo_git_dir,
        prompt: &prompt(t, cfg, n, feedback),
        model: &t.model,
        max_turns: t.max_turns as u32,
        timeout: Duration::from_secs(t.timeout_secs as u64),
        log_path: &log_path,
        sandbox: f.sandbox.as_ref(),
        report: &f.report,
    })
    .await
    .env()?;
    f.report.emit(
        t.id,
        Event::AgentDone {
            exit: outcome.exit_code,
            turns: outcome.num_turns,
            tools: outcome.tool_calls,
            ms: outcome.wall_ms,
            cost: outcome.cost_usd,
            timed_out: outcome.timed_out,
        },
    );

    let verdict = verify::verify(
        Subject {
            task_id: t.id,
            worktree: wt,
            repo_git_dir: &repo_git_dir,
            base_sha: &t.base_sha,
            cfg,
            task_checks: &t.checks,
            sandbox: f.sandbox.as_ref(),
            report: &f.report,
        },
        &outcome,
    )
    .await
    .task()?;

    a.state = verdict.state;
    a.reason = verdict.reason.clone();
    a.finished_at = Some(unix_now());
    a.agent_exit = outcome.exit_code;
    a.timed_out = outcome.timed_out;
    a.num_turns = outcome.num_turns;
    a.tool_calls = outcome.tool_calls;
    a.cost_usd = outcome.cost_usd;
    a.agent_ms = outcome.wall_ms as i64;
    a.commits = verdict.commits;
    a.files_changed = verdict.files_changed;
    a.dirty = verdict.dirty;
    a.verdict_json = serde_json::to_string(&verdict.checks).env()?;
    a.result_text = outcome.result_text.clone();
    f.store.finish_attempt(&a).env()?;
    f.report.emit(
        t.id,
        Event::AttemptDone {
            state: a.state.as_str(),
            reason: &a.reason,
        },
    );
    Ok((a, verdict, outcome))
}
