//! Commands and their terminal output. The engine never prints; this does.

use crate::audit;
use crate::ctx::Forge;
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
    Requests,
    /// Outcomes per workflow version and per step
    Stats,
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
        Cmd::Log { limit } => log(limit),
        Cmd::Show { id } => show(id),
        Cmd::Gc { dry_run } => gc(dry_run).await,
        Cmd::Doctor => run_doctor(),
        Cmd::Trace { id, json } => trace(id, json),
        Cmd::Requests => requests(),
        Cmd::Stats => stats(),
        Cmd::Workflows { json } => list_workflows(json),
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
    let wf = workflows::get(&f.paths.home, &args.workflow)?.with_context(|| {
        format!(
            "unknown workflow {:?}; see `forge workflows`",
            args.workflow
        )
    })?;
    if wf.has(workflows::Step::Tests) {
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

fn list_workflows(json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let all = workflows::load_all(&f.paths.home)?;
    let stats = f.store.workflow_stats()?;
    if json {
        let docs: Vec<serde_json::Value> = all
            .iter()
            .map(|w| {
                let measured: Vec<serde_json::Value> = stats
                    .iter()
                    .filter(|st| st.workflow == w.name)
                    .map(|st| {
                        serde_json::json!({
                            "hash": st.hash, "current": st.hash == w.hash, "tasks": st.tasks, "succeeded": st.succeeded,
                            "failed": st.failed, "blocked": st.blocked, "unverified": st.unverified, "attempts": st.attempts,
                            "cost_usd": st.cost,
                            "success_rate": if st.tasks > 0 { Some(st.succeeded as f64 / st.tasks as f64) } else { None },
                            "cost_per_success_usd": if st.succeeded > 0 { Some(st.cost / st.succeeded as f64) } else { None },
                        })
                    })
                    .collect();
                serde_json::json!({
                    "name": w.name, "hash": w.hash, "description": w.description, "path": w.path,
                    "steps": w.steps.iter().map(|st| serde_json::json!({"kind": st.kind.as_str(), "model": st.model, "max_turns": st.max_turns, "timeout_secs": st.timeout_secs})).collect::<Vec<_>>(),
                    "meta": w.meta,
                    "measured": measured,
                })
            })
            .collect();
        out!("{}", serde_json::to_string_pretty(&docs)?);
        return Ok(());
    }
    for w in &all {
        out!(
            "{:<8} {}  {:<16} {}",
            w.name,
            w.hash,
            w.steps_text(),
            w.description
        );
        for st in &w.steps {
            let mut p = Vec::new();
            if let Some(m) = &st.model {
                p.push(format!("model={m}"));
            }
            if let Some(n) = st.max_turns {
                p.push(format!("max_turns={n}"));
            }
            if let Some(n) = st.timeout_secs {
                p.push(format!("timeout_secs={n}"));
            }
            if !p.is_empty() {
                out!("         {:<8} {}", st.kind.as_str(), p.join(" "));
            }
        }
        out!("         use when   {}", w.meta.use_when);
        out!("         avoid when {}", w.meta.avoid_when);
        if !w.meta.requires.is_empty() {
            out!("         requires   {}", w.meta.requires.join("; "));
        }
        out!(
            "         cost       {:.1}x direct (declared)",
            w.meta.cost_factor
        );
        for st in stats.iter().filter(|st| st.workflow == w.name) {
            out!(
                "         measured   {}{}: {} task(s), {} ok, {} failed, {} blocked, ${:.2} total{}",
                st.hash,
                if st.hash == w.hash {
                    ""
                } else {
                    " (older version)"
                },
                st.tasks,
                st.succeeded,
                st.failed,
                st.blocked,
                st.cost,
                if st.succeeded > 0 {
                    format!(", ${:.2} per success", st.cost / st.succeeded as f64)
                } else {
                    String::new()
                }
            );
        }
        out!("         {}", w.path.display());
    }
    Ok(())
}

fn trace(id: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let Some(t) = f.store.task(id)? else {
        bail!("no task {id}")
    };
    let attempts = f.store.attempts(id)?;
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
                "checks": t.checks, "show_checks": t.show_checks, "allow_protected": t.allow_protected,
                "interface": t.interface, "pushed": t.pushed, "budget_usd": t.budget_usd,
                "created_at": t.created_at, "started_at": t.started_at, "finished_at": t.finished_at,
            },
            "attempts": atts,
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
    for a in &attempts {
        out!();
        out!(
            "=== attempt {} [{}] {}{}",
            a.attempt_no,
            a.step,
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

fn requests() -> Result<()> {
    let f = Forge::open(false, false)?;
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
    }
    Ok(())
}

fn stats() -> Result<()> {
    let f = Forge::open(false, false)?;
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
        "{:<8} {:<8} {:>5} {:>4} {:>6} {:>6} {:>5} {:>6} {:>7} {:>9}",
        "WF",
        "STEP",
        "ATT",
        "OK",
        "AGENTF",
        "CHECKF",
        "ASK",
        "TURNS",
        "SECS",
        "COST"
    );
    for st in f.store.step_stats()? {
        out!(
            "{:<8} {:<8} {:>5} {:>4} {:>6} {:>6} {:>5} {:>6.1} {:>7.0} {:>9}",
            st.workflow,
            st.step,
            st.attempts,
            st.succeeded,
            st.agent_failed,
            st.checks_failed,
            st.needs_input,
            st.mean_turns,
            st.mean_ms / 1000.0,
            format!("${:.2}", st.cost)
        );
    }
    Ok(())
}

fn log(limit: u32) -> Result<()> {
    let f = Forge::open(false, false)?;
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
            if commits > 0 {
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
