//! Commands and their terminal output. The engine never prints; this does.

use crate::audit;
use crate::ctx::Forge;
use crate::profile::{self, LOOKBACK};
use crate::store::{Task, TaskState};
use crate::{config, doctor, engine, git, unix_now, worker, workflows};
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Print to stdout without panicking when the reader has gone away, so
/// `forge log | head` is quiet.
macro_rules! out {
    ($($t:tt)*) => {{
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, $($t)*);
    }};
}

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
    /// Let this task change the repo's [verify] protected paths
    #[arg(long)]
    allow_protected: bool,
    /// Which workflow runs the task (see `forge workflows`)
    #[arg(long, default_value = "direct")]
    workflow: String,
    /// Show the --check commands to the coder (hidden by default)
    #[arg(long)]
    show_checks: bool,
    /// Leave the verified branch pushed for a human instead of landing it on the base branch
    #[arg(long)]
    no_land: bool,
    /// Run only after this task has landed (repeatable); blocked if it ends otherwise
    #[arg(long = "after")]
    after: Vec<i64>,
    /// Do not show the agents the journal of earlier attempts (the control arm of a measurement)
    #[arg(long)]
    no_journal: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run one task now
    Run(TaskArgs),
    /// Queue a task for `forge work`
    Add(TaskArgs),
    /// Run queued tasks: stay up and poll, or drain and exit with --once
    Work {
        /// Tasks to run at the same time
        #[arg(long, default_value_t = 1)]
        jobs: usize,
        /// Seconds between queue polls when idle
        #[arg(long, default_value_t = 30)]
        poll: u64,
        /// Exit when the queue is empty instead of polling
        #[arg(long)]
        once: bool,
        /// Stop after claiming this many tasks
        #[arg(long)]
        max_tasks: Option<u32>,
    },
    /// List tasks, newest first
    Log {
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Re-queue a finished task as a new one: same text, workflow, budget, flags, and dependencies
    Retry {
        id: i64,
        /// Also re-queue everything that waited on it, dependencies mapped to the new ids
        #[arg(long)]
        chain: bool,
        /// Extra attempts after a failure (default: as before)
        #[arg(long)]
        retries: Option<u32>,
        /// Cost cap in USD (default: as before)
        #[arg(long)]
        budget: Option<f64>,
        /// Run a different workflow
        #[arg(long)]
        workflow: Option<String>,
    },
    /// Show one task and its attempts
    Show { id: i64 },
    /// Check this machine can run attempts and nothing is stuck
    Doctor,
    /// List the workflows a task can run, with declared metadata and measured outcomes
    Workflows {
        /// Machine-readable, for an agent choosing a workflow
        #[arg(long)]
        json: bool,
    },
    /// Everything about one task: every step's inputs, outputs, verdict rows, and a diagnosis
    Trace {
        id: i64,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Blocked tasks: questions for the operator and workflow requests
    Requests {
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Outcomes per workflow version and per step
    Stats {
        /// What the agents ran: tools, shell commands, files read, with time, per step
        #[arg(long)]
        tools: bool,
    },
    /// The event log as JSON lines: a client's subscription
    Events {
        /// Byte offset to start from (a snapshot's events_offset)
        #[arg(long)]
        since: Option<u64>,
        /// Keep printing as events arrive
        #[arg(long)]
        follow: bool,
        /// Only this task's events
        #[arg(long)]
        task: Option<i64>,
    },
    /// Tasks, requests, the worker, and the event offset to subscribe from, as one JSON object
    Snapshot,
    /// Merge verified tasks' branches together in order and re-verify after each, without landing:
    /// on success the result is a branch in the repository for a human to fast-forward
    Integrate {
        /// Verified tasks, in merge order
        ids: Vec<i64>,
    },
    /// What every earlier attempt in a task's piece of work said it did, and what the kernel found
    Journal { id: i64 },
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
        Cmd::Work {
            jobs,
            poll,
            once,
            max_tasks,
        } => {
            let f = Arc::new(Forge::open(true, jobs > 1)?);
            worker::work(
                f,
                worker::WorkOpts {
                    jobs,
                    poll: (!once).then_some(poll),
                    max_tasks,
                },
            )
            .await
        }
        Cmd::Log { limit, json } => log(limit, json),
        Cmd::Retry {
            id,
            chain,
            retries,
            budget,
            workflow,
        } => retry(id, chain, retries, budget, workflow).await,
        Cmd::Show { id } => show(id),
        Cmd::Gc { dry_run } => gc(dry_run).await,
        Cmd::Doctor => run_doctor(),
        Cmd::Trace { id, json } => trace(id, json),
        Cmd::Requests { json } => requests(json),
        Cmd::Stats { tools } => stats(tools),
        Cmd::Events {
            since,
            follow,
            task,
        } => events(since, follow, task),
        Cmd::Snapshot => snapshot(),
        Cmd::Integrate { ids } => integrate(ids).await,
        Cmd::Journal { id } => journal(id),
        Cmd::Workflows { json } => list_workflows(json),
    }
}

/// Validate the request and insert a queued task. Refuses a task nothing
/// would verify: a repo with no `[checks]` and a task with no `--check`.
async fn enqueue(f: &Forge, args: &TaskArgs) -> Result<Task> {
    enqueue_with(f, args, None).await
}

async fn enqueue_with(f: &Forge, args: &TaskArgs, retry_of: Option<i64>) -> Result<Task> {
    let repo = args.repo.canonicalize().context("repo path")?;
    if !repo.join(".git").exists() {
        bail!("{} is not a git repository", repo.display());
    }
    let cfg = config::load_working(&repo).await?;
    let wf = workflows::get(&f.paths.home, &args.workflow)?.with_context(|| {
        format!(
            "unknown workflow {:?}; see `forge workflows`",
            args.workflow
        )
    })?;
    // Resolution happens at start; here it only has to be possible, and the
    // whole directory has to be sound: one broken file blocks every task.
    let problems = workflows::check(&f.paths.home)?;
    if let Some(p) = problems.iter().find(|p| p.blocking) {
        bail!(
            "workflow directory is broken: {} {} (forge doctor lists all)",
            p.file,
            p.what
        );
    }
    let resolved = workflows::resolve(&f.paths.home, &args.workflow)?;
    if resolved.steps.iter().any(|s| s.action.name == "tests") {
        if cfg.namespace.is_empty() {
            bail!(
                "the {} workflow needs [verify] namespace in forge.toml: where the tests step may write",
                wf.name
            );
        }
        if !cfg.checks.contains_key("test") {
            bail!(
                "the {} workflow needs a check named `test` in forge.toml: what runs the hidden tests",
                wf.name
            );
        }
    }
    if !cfg.namespace.is_empty() {
        let present = git::ls_tree(&repo, &cfg.base_branch, &cfg.namespace).await?;
        if !present.is_empty() {
            bail!(
                "the verification namespace ({}) must not exist on {}; it is overlaid at verify time. Found: {}",
                cfg.namespace.join(", "),
                cfg.base_branch,
                present.join(", ")
            );
        }
    }
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
        allow_protected: args.allow_protected,
        workflow: args.workflow.clone(),
        workflow_hash: wf.hash.clone(),
        workflow_text: wf.text.clone(),
        show_checks: args.show_checks,
        land: !args.no_land,
        after: args.after.clone(),
        journal: !args.no_journal,
        retry_of,
        ..Default::default()
    };
    for &dep in &t.after {
        let Some(d) = f.store.task(dep)? else {
            bail!("--after {dep}: no such task");
        };
        if d.repo != t.repo {
            bail!(
                "--after {dep}: that task is in {}, not this repository",
                d.repo
            );
        }
        if !d.land && d.state != TaskState::Succeeded {
            bail!(
                "--after {dep}: that task will not land (--no-land), so nothing built on it could see its work"
            );
        }
    }
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

/// A dependency for a re-queued task: the same one if it landed, the
/// newest retry of it if there is one, else a refusal naming it.
fn map_dep(f: &Forge, d: i64, made: &std::collections::HashMap<i64, i64>) -> Result<i64> {
    if let Some(&n) = made.get(&d) {
        return Ok(n);
    }
    let Some(dep) = f.store.task(d)? else {
        bail!("dependency {d} does not exist");
    };
    if dep.state == TaskState::Succeeded && (!dep.land || dep.reason.starts_with("landed ")) {
        return Ok(d);
    }
    if matches!(dep.state, TaskState::Queued | TaskState::Running) {
        return Ok(d);
    }
    if let Some(n) = f.store.latest_retry_of(d)? {
        return Ok(n);
    }
    bail!(
        "dependency {d} ended without landing ({}); retry it first, or retry it with --chain",
        dep.state.as_str()
    )
}

async fn retry(
    id: i64,
    chain: bool,
    retries: Option<u32>,
    budget: Option<f64>,
    workflow: Option<String>,
) -> Result<()> {
    let f = Forge::open(false, false)?;
    let Some(old) = f.store.task(id)? else {
        bail!("no task {id}");
    };
    if matches!(old.state, TaskState::Queued | TaskState::Running) {
        bail!(
            "task {id} is {}; only a finished task is retried",
            old.state.as_str()
        );
    }
    let mut made: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    let mut queue = vec![old];
    let mut first = true;
    while !queue.is_empty() {
        let t = queue.remove(0);
        if made.contains_key(&t.id) {
            continue;
        }
        let after = t
            .after
            .iter()
            .map(|&d| map_dep(&f, d, &made))
            .collect::<Result<Vec<_>>>()?;
        let args = TaskArgs {
            repo: PathBuf::from(&t.repo),
            task: t.task.clone(),
            model: t.model.clone(),
            max_turns: t.max_turns as u32,
            retries: if first {
                retries.unwrap_or((t.max_attempts - 1).max(0) as u32)
            } else {
                (t.max_attempts - 1).max(0) as u32
            },
            timeout_secs: t.timeout_secs as u32,
            budget: if first {
                budget.or(t.budget_usd)
            } else {
                t.budget_usd
            },
            checks: t.checks.clone(),
            allow_protected: t.allow_protected,
            workflow: if first {
                workflow.clone().unwrap_or(t.workflow.clone())
            } else {
                t.workflow.clone()
            },
            show_checks: t.show_checks,
            no_land: !t.land,
            no_journal: !t.journal,
            after,
        };
        let n = enqueue_with(&f, &args, Some(t.id)).await?;
        out!(
            "retried task {} as {}{}",
            t.id,
            n.id,
            if n.after.is_empty() {
                String::new()
            } else {
                format!(
                    " (after {})",
                    n.after
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        );
        made.insert(t.id, n.id);
        first = false;
        if chain {
            queue.extend(f.store.dependents(t.id)?);
        }
    }
    out!("{} queued", f.store.queued_count()?);
    Ok(())
}

async fn add(args: TaskArgs) -> Result<()> {
    let f = Forge::open(false, false)?;
    let t = enqueue(&f, &args).await?;
    out!("queued task {} ({} queued)", t.id, f.store.queued_count()?);
    Ok(())
}

fn run_doctor() -> Result<()> {
    let checks = doctor::run()?;
    let mut failed = false;
    for c in &checks {
        let tag = match c.status {
            doctor::Status::Ok => "OK  ",
            doctor::Status::Warn => "WARN",
            doctor::Status::Fail => {
                failed = true;
                "FAIL"
            }
        };
        out!("{tag} {:<12} {}", c.name, c.detail);
        if !c.hint.is_empty() && c.status != doctor::Status::Ok {
            out!("     {:<12} → {}", "", c.hint);
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

/// Measured profile of a workflow: current version, previous version if
/// any, and all versions together, over the lookback window.
struct Measured {
    current: profile::Profile,
    previous: Option<(String, profile::Profile)>,
    all: profile::Profile,
    regressed: bool,
}

fn measure(f: &Forge, w: &workflows::Workflow) -> Result<Measured> {
    let current = profile::profile(&f.store.runs(&w.name, Some(&w.hash), LOOKBACK)?);
    let all = profile::profile(&f.store.runs(&w.name, None, LOOKBACK)?);
    let previous = f
        .store
        .workflow_versions(&w.name)?
        .into_iter()
        .find(|h| h != &w.hash)
        .map(|h| {
            let p = profile::profile(
                &f.store
                    .runs(&w.name, Some(&h), LOOKBACK)
                    .unwrap_or_default(),
            );
            (h, p)
        });
    let regressed = previous
        .as_ref()
        .is_some_and(|(_, p)| profile::regressed(&current, p));
    Ok(Measured {
        current,
        previous,
        all,
        regressed,
    })
}

fn list_workflows(json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let all = workflows::load_all(&f.paths.home)?;
    let actions = workflows::load_actions(&f.paths.home)?;
    let direct = all
        .iter()
        .find(|w| w.name == "direct")
        .map(|w| measure(&f, w))
        .transpose()?;
    if json {
        let docs: Vec<serde_json::Value> = all
            .iter()
            .map(|w| {
                let m = measure(&f, w).ok();
                let resolved = workflows::resolve(&f.paths.home, &w.name).ok();
                serde_json::json!({
                    "name": w.name, "hash": w.hash, "description": w.description, "path": w.path,
                    "steps": w.steps,
                    "resolved": resolved.as_ref().map(|r| r.steps.iter().map(|s| serde_json::json!({"action": s.action.name, "kind": s.action.kind, "contract": s.action.contract, "hash": s.action.hash, "via": s.via, "model": s.model, "max_turns": s.max_turns, "timeout_secs": s.timeout_secs})).collect::<Vec<_>>()),
                    "meta": w.meta,
                    "measured": m.as_ref().map(|m| serde_json::json!({
                        "current": m.current, "previous": m.previous.as_ref().map(|(h, p)| serde_json::json!({"hash": h, "profile": p})),
                        "all_versions": m.all, "regressed": m.regressed,
                        "cost_vs_direct": match (&direct, m.current.known) {
                            (Some(d), true) if d.current.known && d.current.cost_per_task > 0.0 => Some(m.current.cost_per_task / d.current.cost_per_task),
                            _ => None,
                        },
                    })),
                })
            })
            .collect();
        let acts: Vec<serde_json::Value> = actions
            .values()
            .map(|a| serde_json::json!({"name": a.name, "kind": a.kind, "contract": a.contract, "hash": a.hash, "description": a.description, "consumes": a.consumes, "produces": a.produces, "run": a.run, "check": a.check, "paths": a.paths, "brief": a.brief, "max_turns": a.max_turns, "timeout_secs": a.timeout_secs, "model": a.model}))
            .collect();
        out!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"workflows": docs, "actions": acts, "min_runs_for_known": profile::MIN_N, "lookback": LOOKBACK})
            )?
        );
        return Ok(());
    }
    for w in &all {
        out!(
            "{:<12} {}  {:<24} {}",
            w.name,
            &w.hash[..8],
            w.steps_text(),
            w.description
        );
        match workflows::resolve(&f.paths.home, &w.name) {
            Ok(r) => out!(
                "             resolves   {}",
                r.steps
                    .iter()
                    .map(|s| format!("{}@{}", s.action.name, &s.action.hash[..8]))
                    .collect::<Vec<_>>()
                    .join(" → ")
            ),
            Err(e) => out!("             BROKEN     {e:#}"),
        }
        out!("             use when   {}", w.meta.use_when);
        out!("             avoid when {}", w.meta.avoid_when);
        if !w.meta.requires.is_empty() {
            out!("             requires   {}", w.meta.requires.join("; "));
        }
        let m = measure(&f, w)?;
        out!("             measured   {}", m.current.line());
        if let (Some(d), true) = (&direct, m.current.known)
            && d.current.known
            && d.current.cost_per_task > 0.0
            && w.name != "direct"
        {
            out!(
                "             cost       {:.1}x direct (measured)",
                m.current.cost_per_task / d.current.cost_per_task
            );
        }
        if let Some((h, p)) = &m.previous {
            out!(
                "             previous   {}: {}{}",
                &h[..h.len().min(8)],
                p.line(),
                if m.regressed { "  REGRESSION" } else { "" }
            );
        }
        if m.all.n > m.current.n {
            out!("             all vers.  {}", m.all.line());
        }
        out!("             {}", w.path.display());
    }
    out!();
    for a in actions.values() {
        let what = match (&a.run, &a.check) {
            (Some(r), _) => format!(
                "run {}",
                r.iter()
                    .map(|a| match a.trim().split_once('\n') {
                        // A multi-line script: its first line stands for it.
                        Some((first, _)) => format!("{first} …"),
                        None => a.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            (None, Some(c)) => format!("repo check `{c}`"),
            _ => {
                if a.contract != a.name {
                    format!("contract {}", a.contract)
                } else {
                    String::new()
                }
            }
        };
        let flow = match (a.consumes.is_empty(), a.produces.is_empty()) {
            (true, true) => String::new(),
            _ => format!("  {} → {}", a.consumes.join(","), a.produces.join(",")),
        };
        out!(
            "{:<12} {}  {:<10} {}{}{}",
            a.name,
            &a.hash[..8],
            format!("{:?}", a.kind).to_lowercase(),
            a.description,
            if what.is_empty() {
                String::new()
            } else {
                format!("  [{what}]")
            },
            flow
        );
        if let Some(c) = workflows::commit_for(&f.paths.home, &a.hash) {
            out!("             since      {c}");
        }
    }
    Ok(())
}

fn trace(id: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let Some(t) = f.store.task(id)? else {
        bail!("no task {id}")
    };
    let attempts = f.store.attempts(id)?;
    let ops = f.store.ops(id)?;
    let diagnosis = audit::diagnose(&t, &attempts);
    if json {
        let atts: Vec<serde_json::Value> = attempts
            .iter()
            .map(|a| {
                serde_json::json!({
                    "attempt_no": a.attempt_no, "step": a.step, "state": a.state.as_str(), "reason": a.reason,
                    "started_at": a.started_at, "finished_at": a.finished_at, "agent_exit": a.agent_exit,
                    "timed_out": a.timed_out, "num_turns": a.num_turns, "tool_calls": a.tool_calls,
                    "cost_usd": a.cost_usd, "agent_ms": a.agent_ms, "commits": a.commits,
                    "files_changed": a.files_changed, "dirty": a.dirty, "start_sha": a.start_sha, "end_sha": a.end_sha,
                    "log_path": a.log_path,
                    "tokens": {
                        "input": a.input_tokens, "output": a.output_tokens,
                        "cache_read": a.cache_read_input_tokens, "cache_creation": a.cache_creation_input_tokens,
                    },
                    "inputs": serde_json::from_str::<serde_json::Value>(&a.inputs_json).unwrap_or_default(),
                    "outputs": serde_json::from_str::<serde_json::Value>(&a.outputs_json).unwrap_or_default(),
                    "verdict": serde_json::from_str::<serde_json::Value>(&a.verdict_json).unwrap_or_default(),
                    "envelope": serde_json::from_str::<serde_json::Value>(&a.envelope_json).unwrap_or_default(),
                    "rate_limits": {"five_hour": a.rl_five_hour, "seven_day": a.rl_seven_day},
                })
            })
            .collect();
        let doc = serde_json::json!({
            "task": {
                "id": t.id, "repo": t.repo, "text": t.task, "state": t.state.as_str(), "reason": t.reason,
                "workflow": t.workflow, "workflow_hash": t.workflow_hash, "workflow_text": t.workflow_text,
                "base_branch": t.base_branch, "base_sha": t.base_sha, "branch": t.branch, "worktree": t.worktree,
                "model": t.model, "max_turns": t.max_turns, "max_attempts": t.max_attempts, "timeout_secs": t.timeout_secs,
                "checks": t.checks, "show_checks": t.show_checks, "allow_protected": t.allow_protected, "land": t.land, "after": t.after, "verify_base": t.verify_base, "retry_of": t.retry_of, "journal_enabled": t.journal,
                "parent": t.retry_of, "children": f.store.dependents_retries(t.id)?, "root": f.store.root_of(t.id)?,
                "lineage": f.store.lineage(t.id)?.iter().map(|l| serde_json::json!({"id": l.id, "parent": l.parent, "state": l.state, "reason": l.reason, "workflow": l.workflow, "cost_usd": l.cost})).collect::<Vec<_>>(),
                "journal": crate::engine::journal_for(&f, &t).ok().filter(|j| !j.is_empty()),
                "interface": t.interface, "pushed": t.pushed, "budget_usd": t.budget_usd,
                "created_at": t.created_at, "started_at": t.started_at, "finished_at": t.finished_at,
            },
            "attempts": atts,
            "ops": ops.iter().map(|o| serde_json::json!({"id": o.id, "seq": o.seq, "name": o.name, "kernel": o.kernel, "started_at": o.started_at, "ms": o.ms, "ok": o.ok, "exit": o.exit, "detail": o.detail, "attempt_id": o.attempt_id, "output": o.output})).collect::<Vec<_>>(),
            "resolved": serde_json::from_str::<serde_json::Value>(&t.actions_json).unwrap_or_default(),
            "diagnosis": diagnosis.iter().map(|d| serde_json::json!({"what": d.what, "action": d.action})).collect::<Vec<_>>(),
        });
        out!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    out!("task {}  {}  {}", t.id, t.state.as_str(), t.reason);
    out!("repo       {}", t.repo);
    out!(
        "branch     {} from {} @ {}",
        t.branch,
        t.base_branch,
        t.base_sha
    );
    out!("workflow   {} {}", t.workflow, t.workflow_hash);
    for l in t.workflow_text.lines() {
        out!("  | {l}");
    }
    out!("text       {}", t.task);
    if let Ok(r) = serde_json::from_str::<workflows::Resolved>(&t.actions_json) {
        out!(
            "resolved   {}",
            r.steps
                .iter()
                .map(|s| format!("{}@{}", s.action.name, &s.action.hash[..8]))
                .collect::<Vec<_>>()
                .join(" → ")
        );
        for p in &r.pins {
            out!("  pin      {:<9} {:<10} {}", p.kind, p.name, p.hash);
        }
    }
    for o in ops.iter().filter(|o| o.attempt_id.is_none()) {
        out!(
            "op         {} seq {} {}{} {:.1}s {}",
            if o.ok { "✓" } else { "✗" },
            o.seq,
            o.name,
            if o.kernel { "" } else { " [user]" },
            o.ms as f64 / 1000.0,
            o.detail.lines().next().unwrap_or("")
        );
        for l in o.output.lines().take(12) {
            out!("           > {l}");
        }
    }
    for a in &attempts {
        out!();
        out!(
            "=== attempt {} [{} seq {}] {}{}",
            a.attempt_no,
            a.step,
            a.step_seq,
            a.state.as_str(),
            if a.reason.is_empty() {
                String::new()
            } else {
                format!(": {}", a.reason)
            }
        );
        let inputs: audit::Inputs = serde_json::from_str(&a.inputs_json).unwrap_or_default();
        out!(
            "inputs     model={} max_turns={} timeout={}s base={} start={}",
            inputs.model,
            inputs.max_turns,
            inputs.timeout_secs,
            &inputs.base_sha[..inputs.base_sha.len().min(8)],
            &inputs.start_sha[..inputs.start_sha.len().min(8)]
        );
        out!(
            "           checks_shown={} task_checks={:?} protected={:?} namespace={:?} overlay={:?} prompt_chars={}",
            inputs.checks_shown,
            inputs.task_checks,
            inputs.protected,
            inputs.namespace,
            inputs.overlay_refs,
            inputs.prompt_chars
        );
        if let Some(i) = &inputs.interface {
            out!("interface  {}", i.lines().collect::<Vec<_>>().join(" / "));
        }
        if let Some(fb) = &inputs.feedback {
            out!("feedback   |");
            for l in fb.lines() {
                out!("           | {l}");
            }
        }
        out!(
            "agent      exit {} turns {} tools {} {:.1}s {}{}",
            a.agent_exit.map_or("-".into(), |v| v.to_string()),
            a.num_turns,
            a.tool_calls,
            a.agent_ms as f64 / 1000.0,
            a.cost_usd.map_or("-".into(), |c| format!("${c:.4}")),
            if a.timed_out { " TIMED OUT" } else { "" }
        );
        if let Ok(rows) = serde_json::from_str::<Vec<crate::checks::CheckResult>>(&a.verdict_json) {
            for c in rows {
                out!(
                    "verdict    {} {} {} ({:.1}s){}",
                    if c.ok { "✓" } else { "✗" },
                    c.level,
                    c.name,
                    c.ms as f64 / 1000.0,
                    if c.failing_tests.is_empty() {
                        String::new()
                    } else {
                        format!(" failing: {}", c.failing_tests.join(", "))
                    }
                );
                if !c.ok {
                    for l in crate::checks::last_lines(&c.tail, 40).lines() {
                        out!("           | {l}");
                    }
                }
            }
        }
        let outputs: audit::Outputs = serde_json::from_str(&a.outputs_json).unwrap_or_default();
        out!(
            "outputs    end={} changed={:?} dirty={:?} claims={} checks_run={}",
            &outputs.end_sha[..outputs.end_sha.len().min(8)],
            outputs.changed_files,
            outputs.dirty_files,
            outputs.claims,
            outputs.checks_run
        );
        if let Some(r) = &outputs.verify_ref {
            out!("           verify_ref={r}");
        }
        if !outputs.summary.is_empty() {
            out!(
                "summary    {}",
                outputs.summary.lines().collect::<Vec<_>>().join(" / ")
            );
        }
        out!("log        {}", a.log_path);
    }
    for dgn in &diagnosis {
        out!();
        out!("what       {}", dgn.what);
        out!("action     {}", dgn.action);
    }
    Ok(())
}

fn requests(json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&requests_json(&f)?)?);
        return Ok(());
    }
    let blocked = f.store.blocked()?;
    if blocked.is_empty() {
        out!("no blocked tasks");
        return Ok(());
    }
    out!(
        "{:<5} {:<9} {:<8} {:<18} REQUEST",
        "ID",
        "KIND",
        "WF",
        "REPO"
    );
    for t in blocked {
        let (kind, text) = match t.reason.split_once(": ") {
            Some(("needs workflow", rest)) => ("workflow", rest.to_string()),
            Some(("needs suite", rest)) => ("suite", rest.to_string()),
            Some(("needs input", rest)) => ("question", rest.to_string()),
            _ => ("other", t.reason.clone()),
        };
        let repo_name = Path::new(&t.repo)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out!(
            "{:<5} {:<9} {:<8} {:<18} {}",
            t.id,
            kind,
            t.workflow,
            repo_name,
            text
        );
        let tried = f
            .store
            .attempts(t.id)?
            .last()
            .and_then(|a| serde_json::from_str::<crate::envelope::Envelope>(&a.envelope_json).ok())
            .and_then(|e| e.needs_input)
            .map(|q| q.tried)
            .unwrap_or_default();
        if !tried.is_empty() {
            out!("{:<44} did: {}", "", tried);
        }
    }
    Ok(())
}

fn tool_stats(f: &Forge) -> Result<()> {
    use std::collections::BTreeMap;
    let tasks = f.store.list_tasks(10_000)?;
    // step -> aggregated tools
    let mut per_step: BTreeMap<String, (usize, crate::tools::Tools)> = BTreeMap::new();
    for t in tasks {
        for a in f.store.attempts(t.id)? {
            let Ok(o) = serde_json::from_str::<audit::Outputs>(&a.outputs_json) else {
                continue;
            };
            let Some(tools) = o.tools else {
                continue;
            };
            let e = per_step
                .entry(a.step.clone())
                .or_insert((0, crate::tools::Tools::default()));
            e.0 += 1;
            for (k, u) in tools.by_tool {
                let x = e.1.by_tool.entry(k).or_default();
                x.calls += u.calls;
                x.ms += u.ms;
            }
            for (k, u) in tools.shell {
                let x = e.1.shell.entry(k).or_default();
                x.calls += u.calls;
                x.ms += u.ms;
            }
            for (k, n) in tools.reads {
                *e.1.reads.entry(k).or_default() += n;
            }
        }
    }
    if per_step.is_empty() {
        out!("no attempts with tool facts yet (recorded from the next attempt on)");
        return Ok(());
    }
    for (step, (n, t)) in per_step {
        out!("{step}  ({n} attempt(s))");
        out!(
            "  {:<14} {:>6} {:>9} {:>9}",
            "TOOL",
            "CALLS",
            "TOTAL s",
            "s/CALL"
        );
        for (name, u) in &t.by_tool {
            out!(
                "  {:<14} {:>6} {:>9.1} {:>9.2}",
                name,
                u.calls,
                u.ms as f64 / 1000.0,
                if u.calls > 0 {
                    u.ms as f64 / 1000.0 / u.calls as f64
                } else {
                    0.0
                }
            );
        }
        let mut shell: Vec<_> = t.shell.iter().collect();
        shell.sort_by(|a, b| b.1.ms.cmp(&a.1.ms));
        if !shell.is_empty() {
            out!(
                "  {:<14} {:>6} {:>9} {:>9}",
                "SHELL",
                "CALLS",
                "TOTAL s",
                "s/CALL"
            );
            for (name, u) in shell.iter().take(12) {
                out!(
                    "  {:<14} {:>6} {:>9.1} {:>9.2}",
                    name,
                    u.calls,
                    u.ms as f64 / 1000.0,
                    if u.calls > 0 {
                        u.ms as f64 / 1000.0 / u.calls as f64
                    } else {
                        0.0
                    }
                );
            }
        }
        let mut reads: Vec<_> = t.reads.iter().collect();
        reads.sort_by(|a, b| b.1.cmp(a.1));
        if !reads.is_empty() {
            out!(
                "  most read: {}",
                reads
                    .iter()
                    .take(8)
                    .map(|(p, n)| format!("{p} ({n})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        out!();
    }
    Ok(())
}

fn stats(tools: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    if tools {
        return tool_stats(&f);
    }
    out!(
        "{:<8} {:<16} {:>5} {:>4} {:>4} {:>4} {:>4} {:>5} {:>9} {:>9}",
        "WF",
        "HASH",
        "TASKS",
        "OK",
        "FAIL",
        "BLK",
        "UNV",
        "ATT",
        "COST",
        "$/OK"
    );
    for w in f.store.workflow_stats()? {
        out!(
            "{:<8} {:<16} {:>5} {:>4} {:>4} {:>4} {:>4} {:>5} {:>9} {:>9}",
            w.workflow,
            w.hash,
            w.tasks,
            w.succeeded,
            w.failed,
            w.blocked,
            w.unverified,
            w.attempts,
            format!("${:.2}", w.cost),
            if w.succeeded > 0 {
                format!("${:.2}", w.cost / w.succeeded as f64)
            } else {
                "-".into()
            }
        );
    }
    out!();
    out!(
        "{:<8} {:<8} {:>5} {:>4} {:>6} {:>6} {:>5} {:>6} {:>6} {:>7} {:>9} {:>9}",
        "WF",
        "STEP",
        "ATT",
        "OK",
        "AGENTF",
        "CHECKF",
        "ASK",
        "TURNS",
        "EDIT@",
        "SECS",
        "COST",
        "TOKENS"
    );
    for st in f.store.step_stats()? {
        out!(
            "{:<8} {:<8} {:>5} {:>4} {:>6} {:>6} {:>5} {:>6.1} {:>6} {:>7.0} {:>9} {:>9}",
            st.workflow,
            st.step,
            st.attempts,
            st.succeeded,
            st.agent_failed,
            st.checks_failed,
            st.needs_input,
            st.mean_turns,
            st.mean_first_edit
                .map_or("-".to_string(), |v| format!("{v:.1}")),
            st.mean_ms / 1000.0,
            format!("${:.2}", st.cost),
            st.mean_input_tokens
                .map_or("-".to_string(), |v| format!("{v:.0}"))
        );
    }
    Ok(())
}

fn tasks_json(f: &Forge, limit: u32) -> Result<Vec<serde_json::Value>> {
    Ok(f.store
        .list_tasks(limit)?
        .into_iter()
        .map(|s| serde_json::json!({"id": s.id, "state": s.state, "workflow": s.workflow, "attempts": s.attempts, "cost_usd": s.cost, "created": s.created, "repo": s.repo, "task": s.task}))
        .collect())
}

fn requests_json(f: &Forge) -> Result<Vec<serde_json::Value>> {
    Ok(f.store
        .blocked()?
        .iter()
        .map(|t| {
            let (kind, text) = if t.reason.starts_with("waits on task") {
                ("dependency", t.reason.clone())
            } else {
                match t.reason.split_once(": ") {
                    Some(("needs workflow", rest)) => ("workflow", rest.to_string()),
                    Some(("needs suite", rest)) => ("suite", rest.to_string()),
                    Some(("needs input", rest)) => ("question", rest.to_string()),
                    Some(("review demoted", rest)) => ("review", rest.to_string()),
                    _ => ("other", t.reason.clone()),
                }
            };
            let q = f
                .store
                .attempts(t.id)
                .ok()
                .and_then(|a| a.last().and_then(|a| serde_json::from_str::<crate::envelope::Envelope>(&a.envelope_json).ok()))
                .and_then(|e| e.needs_input);
            serde_json::json!({"id": t.id, "kind": kind, "text": text, "tried": q.as_ref().map(|q| q.tried.clone()).unwrap_or_default(), "path": q.as_ref().map(|q| q.path.clone()).unwrap_or_default(), "workflow": t.workflow, "repo": t.repo, "task": t.task})
        })
        .collect())
}

/// The worker as the pid file says: pid, its binary, whether it is alive,
/// and whether that binary was rebuilt underneath it.
fn worker_json(f: &Forge) -> serde_json::Value {
    let Ok(text) = std::fs::read_to_string(f.paths.home.join("worker.pid")) else {
        return serde_json::json!({"running": false});
    };
    let mut it = text.split_whitespace();
    let pid: i64 = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let exe = it.next().unwrap_or("").to_string();
    let alive = pid > 0 && worker::pid_alive(pid);
    let stale = alive
        && std::fs::read_link(format!("/proc/{pid}/exe"))
            .map(|p| p.to_string_lossy().ends_with(" (deleted)"))
            .unwrap_or(false);
    serde_json::json!({"running": alive, "pid": pid, "exe": exe, "stale_binary": stale})
}

fn journal(id: i64) -> Result<()> {
    let f = Forge::open(false, false)?;
    let Some(t) = f.store.task(id)? else {
        bail!("no task {id}");
    };
    let j = crate::engine::journal_for(&f, &t).map_err(|e| match e {
        crate::engine::Fault::Task(e) | crate::engine::Fault::Env(e) => e,
    })?;
    if j.is_empty() {
        out!("nothing ran before task {id} in its piece of work");
    } else {
        out!("{j}");
    }
    Ok(())
}

/// The integrator's merge-and-verify half, by hand, for a repository that
/// keeps a human at the gate: each task's branch merged onto the base in
/// order, every check with every hidden suite after each, the result left
/// as a branch. A conflict or a red check stops it and says which.
async fn integrate(ids: Vec<i64>) -> Result<()> {
    if ids.is_empty() {
        bail!("name the verified tasks to integrate, in merge order");
    }
    let f = Forge::open(true, false)?;
    let mut tasks = Vec::new();
    for id in &ids {
        let Some(t) = f.store.task(*id)? else {
            bail!("no task {id}");
        };
        if t.state != TaskState::Succeeded && t.state != TaskState::Unverified {
            bail!(
                "task {id} is {}; only a verified task's branch is integrated",
                t.state.as_str()
            );
        }
        tasks.push(t);
    }
    let repo = PathBuf::from(&tasks[0].repo);
    if tasks.iter().any(|t| t.repo != tasks[0].repo) {
        bail!("the tasks are in different repositories");
    }
    let cfg = config::load_working(&repo).await?;
    let stamp = unix_now();
    let branch = format!("forge/integration-{stamp}");
    let dir = f.paths.worktrees.join(format!("integrate-{stamp}"));
    let base_ref = match (
        &cfg.push_remote,
        git::remote_url(&repo, cfg.push_remote.as_deref().unwrap_or("origin")).await,
    ) {
        (Some(name), Some(url)) if git::remote_branch_exists(&url, &cfg.base_branch).await => {
            git::fetch_branch(&repo, name, &cfg.base_branch).await.ok();
            Some(format!("refs/remotes/{name}/{}", cfg.base_branch))
        }
        _ => None,
    };
    let base_sha = git::clone_task(
        &repo,
        &cfg.base_branch,
        &dir,
        &branch,
        base_ref.as_deref(),
        None,
    )
    .await?;
    out!("base     {} @ {}", cfg.base_branch, &base_sha[..8]);
    let remote_url = match &cfg.push_remote {
        Some(name) => git::remote_url(&repo, name).await,
        None => None,
    };
    let mut merged: Vec<i64> = Vec::new();
    let mut overlay: Vec<String> = Vec::new();
    if git::ref_exists(&repo, "refs/heads/forge-verify").await {
        overlay.push("forge-verify".into());
    }
    let cfg_base = config::load_at(&repo, &dir, &base_sha).await?;
    for t in &tasks {
        // Where the branch lives: the repository, else the remote, else the worktree.
        let src = if git::ref_exists(&repo, &format!("refs/heads/{}", t.branch)).await {
            repo.display().to_string()
        } else if let Some(url) = &remote_url
            && git::remote_branch_exists(url, &t.branch).await
        {
            url.clone()
        } else if Path::new(&t.worktree).join(".git").exists() {
            t.worktree.clone()
        } else {
            bail!(
                "task {}'s branch {} is nowhere: not in the repository, the remote, or a worktree",
                t.id,
                t.branch
            );
        };
        git::fetch_ref(&dir, &src, &t.branch)
            .await
            .with_context(|| format!("fetching {} from {src}", t.branch))?;
        match git::merge(
            &dir,
            "FETCH_HEAD",
            &format!("Integrate task {}: {}", t.id, t.branch),
        )
        .await?
        {
            git::Merge::UpToDate => out!("task {:<4} already contained", t.id),
            git::Merge::Merged(sha) => {
                out!("task {:<4} merged {} as {}", t.id, t.branch, &sha[..8])
            }
            git::Merge::Conflict(files) => {
                out!("task {:<4} CONFLICT in {}", t.id, files.join(", "));
                out!(
                    "stopped after {} task(s); the scratch clone is at {}",
                    merged.len(),
                    dir.display()
                );
                bail!(
                    "task {} conflicts with what came before it: {}",
                    t.id,
                    files.join(", ")
                );
            }
        }
        merged.push(t.id);
        let own = format!("verify/{}", t.id);
        if git::ref_exists(&repo, &format!("refs/heads/{own}")).await {
            overlay.push(own);
        }
        let v = crate::verify::verify_integration(&crate::verify::Subject {
            task_id: t.id,
            repo: &repo,
            worktree: &dir,
            base_sha: &base_sha,
            start_sha: &base_sha,
            cfg: &cfg_base,
            task_checks: &[],
            paths: &[],
            allow_protected: true,
            overlay_refs: &overlay,
            pending_main: None,
            sandbox: f.sandbox.as_ref(),
            report: &f.report,
        })
        .await?;
        if v.state != crate::store::AttemptState::Succeeded {
            out!("task {:<4} checks FAIL after merging: {}", t.id, v.reason);
            out!(
                "stopped after {} task(s); the scratch clone is at {}",
                merged.len(),
                dir.display()
            );
            bail!(
                "the tree with task {} merged does not verify: {}",
                t.id,
                v.reason
            );
        }
        out!("task {:<4} verified with everything before it", t.id);
    }
    git::push_to_repo(&dir, &repo, &branch).await?;
    let _ = std::fs::remove_dir_all(&dir);
    out!(
        "integrated {} task(s) as {branch} in {}\n  git -C {} merge --ff-only {branch}",
        merged.len(),
        repo.display(),
        repo.display()
    );
    Ok(())
}

fn snapshot() -> Result<()> {
    let f = Forge::open(false, false)?;
    let offset = std::fs::metadata(f.paths.home.join("events.jsonl"))
        .map(|m| m.len())
        .unwrap_or(0);
    let doc = serde_json::json!({
        "tasks": tasks_json(&f, 200)?,
        "requests": requests_json(&f)?,
        "worker": worker_json(&f),
        "events_offset": offset,
    });
    out!("{}", serde_json::to_string_pretty(&doc)?);
    Ok(())
}

fn events(since: Option<u64>, follow: bool, task: Option<i64>) -> Result<()> {
    use std::io::{BufRead, Seek};
    let paths = crate::ctx::Paths::resolve()?;
    let path = paths.home.join("events.jsonl");
    let mut pos = since.unwrap_or(0);
    let mut stdout = std::io::stdout().lock();
    loop {
        if let Ok(mut file) = std::fs::File::open(&path) {
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            if len < pos {
                // Rolled: start over from the new file.
                pos = 0;
            }
            file.seek(std::io::SeekFrom::Start(pos))?;
            let mut reader = std::io::BufReader::new(file);
            let mut line = String::new();
            loop {
                line.clear();
                let n = reader.read_line(&mut line)?;
                if n == 0 || !line.ends_with('\n') {
                    break;
                }
                pos += n as u64;
                if let Some(id) = task
                    && serde_json::from_str::<serde_json::Value>(&line)
                        .ok()
                        .and_then(|v| v["task"].as_i64())
                        != Some(id)
                {
                    continue;
                }
                use std::io::Write;
                if writeln!(stdout, "{}", line.trim_end()).is_err() {
                    return Ok(());
                }
            }
            use std::io::Write;
            let _ = stdout.flush();
        }
        if !follow {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

fn log(limit: u32, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&tasks_json(&f, limit)?)?);
        return Ok(());
    }
    out!(
        "{:<5} {:<11} {:<7} {:<3} {:<8} {:<19} {:<18} TASK",
        "ID",
        "STATE",
        "WF",
        "ATT",
        "COST",
        "CREATED",
        "REPO"
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
        out!(
            "{:<5} {:<11} {:<7} {:<3} {:<8} {:<19} {:<18} {}",
            s.id,
            s.state,
            s.workflow,
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
    out!("task       {}", t.id);
    out!(
        "state      {}{}",
        t.state.as_str(),
        if t.reason.is_empty() {
            String::new()
        } else {
            format!(" ({})", t.reason)
        }
    );
    out!("repo       {}", t.repo);
    out!(
        "base       {} @ {}",
        t.base_branch,
        if t.base_sha.is_empty() {
            "-"
        } else {
            &t.base_sha[..8]
        }
    );
    out!(
        "branch     {}{}",
        if t.branch.is_empty() { "-" } else { &t.branch },
        if t.pushed { " (pushed)" } else { "" }
    );
    out!(
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
    out!(
        "model      {} (max {} turns, max {} attempts, {}s timeout)",
        t.model,
        t.max_turns,
        t.max_attempts,
        t.timeout_secs
    );
    out!(
        "cost       ${cost:.4} over {} attempt(s){}",
        attempts.len(),
        t.budget_usd
            .map_or(String::new(), |b| format!(" (task cap ${b:.2})"))
    );
    for c in &t.checks {
        out!("check      $ {c}");
    }
    if t.allow_protected {
        out!("protected  changes allowed");
    }
    if !t.land {
        out!("land       manual: the verified branch is left for a human");
    }
    if !t.after.is_empty() {
        out!(
            "after      {}",
            t.after
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(r) = t.retry_of {
        out!("retry of   {r}");
    }
    let lineage = f.store.lineage(t.id)?;
    if lineage.len() > 1 {
        out!(
            "lineage    {}",
            lineage
                .iter()
                .map(|l| if l.id == t.id {
                    format!("[{} {}]", l.id, l.state)
                } else {
                    format!("{} {}", l.id, l.state)
                })
                .collect::<Vec<_>>()
                .join(" → ")
        );
    }
    out!("workflow   {} {}", t.workflow, t.workflow_hash);
    if !t.interface.is_empty() {
        out!(
            "interface  {}",
            t.interface.lines().collect::<Vec<_>>().join(" / ")
        );
    }
    out!("text       {}", t.task);
    for a in &attempts {
        out!();
        out!(
            "attempt {} [{}]  {}{}  {}  {} turns  {} tools  {:.1}s  {}  {} commit(s)  {} file(s){}",
            a.attempt_no,
            a.step,
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
        out!("  log     {}", a.log_path);
        if let Ok(o) = serde_json::from_str::<audit::Outputs>(&a.outputs_json)
            && let Some(t) = o.tools
        {
            out!("  ran     {}", t.line());
        }
        if let Ok(checks) = serde_json::from_str::<Vec<crate::checks::CheckResult>>(&a.verdict_json)
        {
            for c in checks {
                out!(
                    "  {} {} {} ({:.1}s){}",
                    if c.ok { "✓" } else { "✗" },
                    c.level,
                    c.name,
                    c.ms as f64 / 1000.0,
                    if c.failing_tests.is_empty() {
                        String::new()
                    } else {
                        format!("  failing: {}", c.failing_tests.join(", "))
                    }
                );
            }
        }
        if let Ok(Some(e)) = crate::envelope::parse(Some(&a.envelope_json), "") {
            out!(
                "  reported {} change(s), {} check(s) run, {} claim(s)",
                e.changes.len(),
                e.checks_run.len(),
                e.claims.len()
            );
            for c in &e.claims {
                out!("    claim   {} [{}]", c.claim, c.evidence);
            }
            if let Some(q) = &e.needs_input {
                out!("    QUESTION {}", q.question);
            }
        }
        if a.rl_five_hour.is_some() || a.rl_seven_day.is_some() {
            out!(
                "  usage   5h {} · 7d {}",
                a.rl_five_hour
                    .map_or("-".into(), |u| format!("{:.0}%", u * 100.0)),
                a.rl_seven_day
                    .map_or("-".into(), |u| format!("{:.0}%", u * 100.0))
            );
        }
        if !a.result_text.is_empty() {
            let first: String = a
                .result_text
                .lines()
                .take(3)
                .collect::<Vec<_>>()
                .join(" / ");
            out!("  result  {}", first.chars().take(200).collect::<String>());
        }
    }
    for dgn in audit::diagnose(&t, &attempts) {
        out!();
        out!("what       {}", dgn.what);
        out!("action     {}", dgn.action);
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
        let verdict: Result<Result<(), String>> = async {
            if !wt.exists() {
                return Ok(Ok(()));
            }
            if t.state == TaskState::Running {
                return Ok(Err("still running".into()));
            }
            if !git::dirty_paths(wt).await?.is_empty() {
                return Ok(Err("uncommitted changes".into()));
            }
            let commits = git::count_commits(wt, &t.base_sha).await?;
            // Unpublished commits are kept, unless a later try of the same
            // piece of work succeeded: then they are superseded, not lost.
            let superseded = f
                .store
                .lineage(t.id)?
                .iter()
                .any(|l| l.id > t.id && l.state == "succeeded");
            if commits > 0 && !superseded {
                let repo = Path::new(&t.repo);
                let url = match config::load_working(repo).await?.push_remote {
                    Some(name) => git::remote_url(repo, &name).await,
                    None => None,
                };
                let published = match url {
                    Some(u) => git::published(wt, &u, &t.branch).await?,
                    None => false,
                };
                if !published {
                    return Ok(Err(format!("{commits} commit(s) not on the remote")));
                }
            }
            if !dry_run {
                std::fs::remove_dir_all(wt)?;
                let _ = std::fs::remove_dir_all(engine::tests_clone_dir(&t.worktree));
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
                out!(
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
                out!("task {:<4} kept ({reason})", t.id);
                out!("           rm -rf {}", t.worktree);
            }
            Err(e) => {
                kept += 1;
                out!("task {:<4} kept (error: {e:#})", t.id);
            }
        }
    }
    out!(
        "{} {removed}, kept {kept}",
        if dry_run { "would remove" } else { "removed" }
    );
    Ok(())
}
