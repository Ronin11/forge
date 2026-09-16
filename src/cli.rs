//! Commands and their terminal output. The engine never prints; this does.

use crate::audit;
use crate::ctx::Forge;
use crate::profile::{self, LOOKBACK};
use crate::store::{Task, TaskState};
use crate::{config, doctor, git, unix_now, worker, workflows};
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
    /// Model for every step (default: the provider's own default model)
    #[arg(long)]
    model: Option<String>,
    /// Agent backend every role of this task runs under (code, tests,
    /// review, plan, and the supervisor), overriding the project's and
    /// the operator's [roles] table; from [providers.<name>] in the
    /// operator config
    #[arg(long)]
    provider: Option<String>,
    /// Turns per attempt, a runaway guard; cost, wall time and the rate windows bound the work
    #[arg(long, default_value_t = 100)]
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
    /// Which workflow runs the task (default: the project's, else "direct")
    #[arg(long)]
    workflow: Option<String>,
    /// The project this task belongs to (default: the repository's own project)
    #[arg(long)]
    project: Option<String>,
    /// The initiative this task belongs to; its project applies (must
    /// agree with --project when both are given)
    #[arg(long)]
    initiative: Option<i64>,
    /// Show the --check commands to the coder (hidden by default)
    #[arg(long)]
    show_checks: bool,
    /// Leave the verified branch pushed for a human instead of landing it on the base branch
    #[arg(long)]
    no_land: bool,
    /// Run only after this task has landed (repeatable); blocked if it ends otherwise
    #[arg(long = "after")]
    after: Vec<i64>,
    /// Show the agents the journal of earlier attempts, overriding the
    /// operator's control-arm fraction for this task
    #[arg(long, conflicts_with = "no_journal")]
    journal: bool,
    /// Do not show the agents the journal of earlier attempts, overriding
    /// the operator's control-arm fraction for this task
    #[arg(long)]
    no_journal: bool,
    /// Do not show the agents the repository map from the context operation (the control arm)
    #[arg(long)]
    no_context: bool,
    /// After an attempt fails its checks, continue the next attempt in the
    /// same CLI session instead of starting a fresh one
    #[arg(long)]
    resume_on_failure: bool,
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
        /// Only tasks in this state (queued, running, succeeded, failed, blocked, unverified, withdrawn)
        #[arg(long)]
        state: Option<String>,
        /// Only tasks in this repository
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Only tasks with ids below this one: the next page when scrolling back
        #[arg(long)]
        before: Option<i64>,
        /// Only tasks whose text contains this, or whose id is exactly this
        #[arg(long)]
        grep: Option<String>,
        /// Only tasks that ran this workflow
        #[arg(long)]
        workflow: Option<String>,
        /// Only tasks in this project
        #[arg(long)]
        project: Option<String>,
        /// Only tasks in this initiative
        #[arg(long)]
        initiative: Option<i64>,
    },
    /// Re-queue a finished task as a new one: same text, workflow, budget, flags, and dependencies
    Retry {
        id: i64,
        /// Accepted for compatibility: dependents follow a retry on their own
        #[arg(long)]
        chain: bool,
        /// Extra attempts after a failure (default: as before)
        #[arg(long)]
        retries: Option<u32>,
        /// Cost cap in USD (default: as before)
        #[arg(long)]
        budget: Option<f64>,
        /// Turns per attempt (default: as before)
        #[arg(long)]
        max_turns: Option<u32>,
        /// Wall-clock limit per attempt in seconds (default: as before)
        #[arg(long)]
        timeout_secs: Option<u32>,
        /// Run a different workflow
        #[arg(long)]
        workflow: Option<String>,
    },
    /// Answer a task blocked on a question and re-queue it as a retry
    Answer {
        id: i64,
        /// The answer, appended to the task's text for the re-queued attempt
        text: String,
    },
    /// Withdraw a blocked or queued task the operator has decided not to
    /// do: written against a stale description, superseded, or the
    /// product decision went the other way. Terminal; refused on a
    /// running or landed task.
    Withdraw {
        id: i64,
        /// Why: recorded as the task's reason and as a decision row
        #[arg(long)]
        reason: String,
        /// Who decided
        #[arg(long, default_value = "operator")]
        by: String,
    },
    /// List recorded operator answers, newest first
    Decisions {
        /// Only decisions for this repository
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Only decisions on this project's tasks
        #[arg(long)]
        project: Option<String>,
        /// Only decisions on this initiative's tasks
        #[arg(long)]
        initiative: Option<i64>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Show one task and its attempts
    Show { id: i64 },
    /// Run the supervisor on a task blocked with a question, now
    Supervise { id: i64 },
    /// Check this machine can run attempts and nothing is stuck
    Doctor {
        /// Machine-readable: a JSON array of {name, status, detail, hint}
        #[arg(long)]
        json: bool,
    },
    /// Print the crate version and, if built from a git checkout, its commit
    Version,
    /// List the workflows a task can run, with declared metadata and measured outcomes
    Workflows {
        /// Machine-readable, for an agent choosing a workflow
        #[arg(long)]
        json: bool,
    },
    /// List the configured agent backends (`[providers.<name>]`), with their runner and model
    Providers {
        /// Machine-readable
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
        /// Only requests for this repository
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Outcomes per workflow version and per step
    Stats {
        /// What the agents ran: tools, shell commands, files read, with time, per step
        #[arg(long)]
        tools: bool,
        /// With --tools, only this step's section
        #[arg(long)]
        step: Option<String>,
        /// Defect escape per workflow: landed tasks that broke the next
        /// task's base, or were later repaired, each as a share of landed
        #[arg(long)]
        quality: bool,
        /// The journal control arm's retrospective split: code attempts
        /// after the first, by whether they were handed a journal
        #[arg(long)]
        journal: bool,
        /// Attempts, outcomes, cost and wall time per (role, provider,
        /// model), role being the attempt step
        #[arg(long)]
        by_role: bool,
        /// Only this project's tasks (also adds the per-project section
        /// when neither this nor --initiative is given)
        #[arg(long)]
        project: Option<String>,
        /// Only this initiative's tasks
        #[arg(long)]
        initiative: Option<i64>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
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
    /// Land an already-verified task's branch through the integrator: merge the base in, re-verify, push, fast-forward
    Land { id: i64 },
    /// Merge verified tasks' branches together in order and re-verify after each, without landing:
    /// on success the result is a branch in the repository for a human to fast-forward
    Integrate {
        /// Verified tasks, in merge order
        ids: Vec<i64>,
    },
    /// What every earlier attempt in a task's piece of work said it did, and what the kernel found
    Journal {
        id: i64,
        /// Machine-readable: a JSON array of {task, attempt, step, state, said, found}
        #[arg(long)]
        json: bool,
    },
    /// Remove worktrees that are clean and whose commits are all on a remote
    Gc {
        /// Report what would happen without removing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Plugins: integrations discovered under FORGE2_HOME/plugins and plugin_dirs
    Plugin {
        #[command(subcommand)]
        cmd: PluginCmd,
    },
    /// External references on a task: the pull request it landed as, the issue it came from
    Ref {
        #[command(subcommand)]
        cmd: RefCmd,
    },
    /// Projects: the unit of ownership above a task (see docs/PROJECTS.md)
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Initiatives: one outcome, pursued as a set of tasks, tracked as
    /// one thing (see docs/PROJECTS.md)
    Initiative {
        #[command(subcommand)]
        cmd: InitiativeCmd,
    },
    /// Run a deploy target now, or (with `log`) show what was deployed
    /// when (see docs/DEPLOY.md)
    Deploy(DeployArgs),
}

#[derive(Args)]
pub struct DeployArgs {
    #[command(subcommand)]
    cmd: Option<DeploySub>,
    /// The project the target belongs to (omit only with `log`)
    project: Option<String>,
    /// The target to deploy (omit only with `log`)
    name: Option<String>,
    /// The commit to deploy (default: the project repository's current HEAD)
    #[arg(long)]
    sha: Option<String>,
}

#[derive(Subcommand)]
enum DeploySub {
    /// What was deployed when, and what the check said
    Log {
        project: String,
        /// Only this target's deploys
        name: Option<String>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum PluginCmd {
    /// Every plugin found, where it came from, enabled or not
    List {
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Whether a plugin (or every plugin) is enabled
    Status {
        name: Option<String>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Enable a plugin: a running `forge work` notices within a few
    /// seconds and starts it, no restart needed
    Enable { name: String },
    /// Disable a plugin: a running `forge work` notices within a few
    /// seconds and stops it, no restart needed
    Disable { name: String },
    /// Copy a plugin directory into FORGE2_HOME/plugins and run its build
    Install {
        /// The plugin's own directory, holding plugin.toml
        path: PathBuf,
    },
    /// Stop a plugin, clear its enabled flag, and remove the installed
    /// copy; its FORGE2_HOME/plugins-state is left alone
    Uninstall { name: String },
    /// A plugin's stdout/stderr log
    Logs {
        name: String,
        /// Keep printing as the log grows
        #[arg(long, short)]
        follow: bool,
    },
}

#[derive(Subcommand)]
enum RefCmd {
    /// Record a reference on a task: the pull request it landed as, the issue it came from
    Add {
        task: i64,
        /// What kind of reference it is, e.g. "pr" or "issue"
        #[arg(long)]
        kind: String,
        #[arg(long)]
        url: String,
        /// Free text, e.g. the PR's title
        #[arg(long, default_value = "")]
        label: String,
        /// Who recorded it: "operator" by default, or a plugin's own name
        #[arg(long, default_value = "operator")]
        by: String,
    },
    /// A task's references
    List {
        task: i64,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Register a new project: what is being built, and for whom
    New {
        name: String,
        /// One paragraph saying what the project is for
        #[arg(long)]
        purpose: String,
        /// A repository the project works in, optionally with the paths
        /// it owns there: `<path>` or `<path>:<scope1>,<scope2>` (repeatable)
        #[arg(long = "repo")]
        repos: Vec<String>,
    },
    /// Every project, its repositories, task counts by state, and cost
    List {
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// One project's repositories, task counts by state, and cost
    Show {
        name: String,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Set a project's defaults: what initiatives and tasks inherit
    /// unless they say otherwise (see docs/PROJECTS.md, "Defaults")
    Set {
        name: String,
        /// Which workflow a task in this project runs by default
        #[arg(long)]
        workflow: Option<String>,
        /// Default per-task cost cap in USD
        #[arg(long = "per-task-usd")]
        per_task_usd: Option<f64>,
        /// Default per-initiative cost cap in USD
        #[arg(long = "per-initiative-usd")]
        per_initiative_usd: Option<f64>,
        /// Default supervisor model
        #[arg(long = "supervisor-model")]
        supervisor_model: Option<String>,
        /// Default supervisor answers per lineage before a question reaches the operator
        #[arg(long = "supervisor-per-lineage")]
        supervisor_per_lineage: Option<u32>,
        /// Extra protected paths, on top of each repository's own forge.toml (repeatable)
        #[arg(long = "protected")]
        protected: Vec<String>,
        /// Which provider a role runs under in this project, as
        /// `<role>=<provider>` (role: code, tests, review, plan,
        /// supervisor; repeatable). Overrides the operator's [roles]
        /// table; a task's own --provider overrides this.
        #[arg(long = "role")]
        role: Vec<String>,
    },
    /// This project's backlog: things worth doing that are not yet queued
    Backlog {
        name: String,
        /// Add a backlog item
        #[arg(long)]
        add: Option<String>,
        /// Mark a backlog item done, by id
        #[arg(long)]
        done: Option<i64>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Deploy targets: where this project's landed code runs (see docs/DEPLOY.md)
    Deploy {
        #[command(subcommand)]
        cmd: ProjectDeployCmd,
    },
}

#[derive(Subcommand)]
enum ProjectDeployCmd {
    /// Declare a deploy target: where landed code runs, how it gets
    /// there, and what proves it is up
    Add {
        project: String,
        name: String,
        /// The repository this target deploys
        #[arg(long)]
        repo: PathBuf,
        /// Paths within the repository this target owns, comma-separated
        /// (default: the whole repository)
        #[arg(long)]
        scope: Option<String>,
        /// The action file this target runs, e.g. "deploy-command"
        #[arg(long)]
        method: String,
        /// An argument to the method, as `<key>=<value>` (repeatable)
        #[arg(long = "arg")]
        args: Vec<String>,
        /// A shell command, run where the thing runs, whose exit status
        /// is the deploy's verdict. Required, except for deploy-static,
        /// which defaults to fetching a url arg and requiring 200
        #[arg(long)]
        check: Option<String>,
        /// Run this target automatically after a landing on its repository
        #[arg(long)]
        on_landing: bool,
    },
    /// A project's deploy targets
    List {
        project: String,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum InitiativeCmd {
    /// Register a new initiative and, with --from, file its tasks
    New {
        project: String,
        /// One sentence saying what is true when the initiative is done;
        /// placed in every task's prompt as "Why this task exists"
        #[arg(long)]
        outcome: String,
        /// A file of task texts, one per paragraph (blank-line
        /// separated); a paragraph may lead with `after: <n>` (an
        /// earlier paragraph's 1-based number, as a dependency),
        /// `repo: <path>` (else the project's first repository) and
        /// `provider: <name>` (else --provider's default)
        #[arg(long)]
        from: Option<PathBuf>,
        /// The provider every paragraph runs under unless it names its
        /// own `provider:` (default: "anthropic"); validated against
        /// `[providers.<name>]` (see `forge providers`) when the file is
        /// read
        #[arg(long)]
        provider: Option<String>,
        /// This initiative's own cost cap in USD (default: the
        /// project's per-initiative-usd)
        #[arg(long)]
        budget: Option<f64>,
        /// Hold the initiative after this many of its tasks fail in a
        /// row on the same L0 rule (default: 3)
        #[arg(long = "stop-after")]
        stop_after: Option<u32>,
    },
    /// File a task's recorded plan (from the investigate directive) into
    /// a new initiative: one task per plan item, chained in order,
    /// against the task's repository, in the task's project
    FromPlan {
        /// The task whose `t.plan` is filed; refused if it has none
        task: i64,
        /// The initiative's outcome (default: the task's own text)
        #[arg(long)]
        outcome: Option<String>,
    },
    /// Every initiative, its state, task counts and cost
    List {
        /// Only this project's
        project: Option<String>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// One initiative: its state, task counts, cost and settings
    Show {
        id: i64,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// The generated report: outcome, each task's fate, what
    /// verification refused, supervisor rulings, questions that reached
    /// the operator, cost and elapsed time
    Report {
        id: i64,
        /// Machine-readable
        #[arg(long)]
        json: bool,
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
        Cmd::Log {
            limit,
            json,
            state,
            repo,
            before,
            grep,
            workflow,
            project,
            initiative,
        } => log(
            LogArgs {
                limit,
                state,
                repo,
                before,
                grep,
                workflow,
                project,
                initiative,
            },
            json,
        ),
        Cmd::Retry {
            id,
            chain,
            retries,
            budget,
            max_turns,
            timeout_secs,
            workflow,
        } => {
            retry(
                id,
                chain,
                crate::queue::RetryOverrides {
                    retries,
                    budget,
                    max_turns,
                    timeout_secs,
                    workflow,
                },
            )
            .await
        }
        Cmd::Answer { id, text } => answer(id, text).await,
        Cmd::Withdraw { id, reason, by } => withdraw(id, reason, by),
        Cmd::Decisions {
            repo,
            project,
            initiative,
            json,
        } => decisions(repo, project, initiative, json),
        Cmd::Show { id } => show(id),
        Cmd::Supervise { id } => supervise_now(id).await,
        Cmd::Gc { dry_run } => gc(dry_run).await,
        Cmd::Doctor { json } => run_doctor(json),
        Cmd::Version => version(),
        Cmd::Trace { id, json } => trace(id, json),
        Cmd::Requests { repo, json } => requests(repo, json),
        Cmd::Stats {
            tools,
            step,
            quality,
            journal,
            by_role,
            project,
            initiative,
            json,
        } => stats(
            tools, step, quality, journal, by_role, project, initiative, json,
        ),
        Cmd::Events {
            since,
            follow,
            task,
        } => events(since, follow, task),
        Cmd::Snapshot => snapshot(),
        Cmd::Integrate { ids } => integrate(ids).await,
        Cmd::Land { id } => land(id).await,
        Cmd::Journal { id, json } => journal(id, json),
        Cmd::Workflows { json } => list_workflows(json),
        Cmd::Providers { json } => list_providers(json),
        Cmd::Plugin { cmd } => match cmd {
            PluginCmd::List { json } => plugin_list(json),
            PluginCmd::Status { name, json } => plugin_status(name, json),
            PluginCmd::Enable { name } => plugin_set_enabled(name, true),
            PluginCmd::Disable { name } => plugin_set_enabled(name, false),
            PluginCmd::Install { path } => plugin_install(path),
            PluginCmd::Uninstall { name } => plugin_uninstall(name),
            PluginCmd::Logs { name, follow } => plugin_logs(name, follow),
        },
        Cmd::Ref { cmd } => match cmd {
            RefCmd::Add {
                task,
                kind,
                url,
                label,
                by,
            } => ref_add(task, kind, url, label, by),
            RefCmd::List { task, json } => ref_list(task, json),
        },
        Cmd::Project { cmd } => match cmd {
            ProjectCmd::New {
                name,
                purpose,
                repos,
            } => project_new(name, purpose, repos),
            ProjectCmd::List { json } => project_list(json),
            ProjectCmd::Show { name, json } => project_show(name, json),
            ProjectCmd::Set {
                name,
                workflow,
                per_task_usd,
                per_initiative_usd,
                supervisor_model,
                supervisor_per_lineage,
                protected,
                role,
            } => project_set(
                name,
                workflow,
                per_task_usd,
                per_initiative_usd,
                supervisor_model,
                supervisor_per_lineage,
                protected,
                role,
            ),
            ProjectCmd::Backlog {
                name,
                add,
                done,
                json,
            } => project_backlog(name, add, done, json),
            ProjectCmd::Deploy { cmd } => match cmd {
                ProjectDeployCmd::Add {
                    project,
                    name,
                    repo,
                    scope,
                    method,
                    args,
                    check,
                    on_landing,
                } => {
                    project_deploy_add(project, name, repo, scope, method, args, check, on_landing)
                }
                ProjectDeployCmd::List { project, json } => project_deploy_list(project, json),
            },
        },
        Cmd::Deploy(a) => match a.cmd {
            Some(DeploySub::Log {
                project,
                name,
                json,
            }) => deploy_log(project, name, json),
            None => deploy_run(a.project, a.name, a.sha).await,
        },
        Cmd::Initiative { cmd } => match cmd {
            InitiativeCmd::New {
                project,
                outcome,
                from,
                provider,
                budget,
                stop_after,
            } => initiative_new(project, outcome, from, provider, budget, stop_after).await,
            InitiativeCmd::FromPlan { task, outcome } => initiative_from_plan(task, outcome).await,
            InitiativeCmd::List { project, json } => initiative_list(project, json),
            InitiativeCmd::Show { id, json } => initiative_show(id, json),
            InitiativeCmd::Report { id, json } => initiative_report(id, json),
        },
    }
}

impl From<&TaskArgs> for crate::queue::TaskRequest {
    fn from(a: &TaskArgs) -> Self {
        crate::queue::TaskRequest {
            repo: a.repo.clone(),
            task: a.task.clone(),
            model: a.model.clone(),
            provider: a.provider.clone(),
            max_turns: a.max_turns,
            retries: a.retries,
            timeout_secs: a.timeout_secs,
            budget: a.budget,
            checks: a.checks.clone(),
            allow_protected: a.allow_protected,
            workflow: a.workflow.clone(),
            project: a.project.clone(),
            initiative: a.initiative,
            show_checks: a.show_checks,
            no_land: a.no_land,
            after: a.after.clone(),
            journal_choice: if a.journal {
                Some(true)
            } else if a.no_journal {
                Some(false)
            } else {
                None
            },
            no_context: a.no_context,
            resume_on_failure: a.resume_on_failure,
        }
    }
}

async fn enqueue(f: &Forge, args: &TaskArgs) -> Result<Task> {
    crate::queue::enqueue(f, &args.into(), None).await
}

/// Answer a task blocked on a question, as the operator.
async fn answer(id: i64, text: String) -> Result<()> {
    let f = Forge::open(false, false)?;
    let (_, n) = crate::queue::answer(&f, id, &text, "operator", "").await?;
    out!("answered task {id} as {}", n.id);
    Ok(())
}

/// Withdraw a blocked or queued task, as the operator.
fn withdraw(id: i64, reason: String, by: String) -> Result<()> {
    let f = Forge::open(false, false)?;
    crate::queue::withdraw(&f, id, &reason, &by)?;
    out!("withdrew task {id}: {reason}");
    Ok(())
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

async fn retry(id: i64, chain: bool, o: crate::queue::RetryOverrides) -> Result<()> {
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
            .map(|&d| crate::queue::map_dep(&f, d, &made))
            .collect::<Result<Vec<_>>>()?;
        let args = crate::queue::retry_request(&t, &o, first, after, None);
        let n = crate::queue::enqueue(&f, &args, Some(t.id)).await?;
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
        // Dependents follow a retry on their own now (enqueue_with reroutes
        // them); `--chain` is kept for callers that still pass it.
        let _ = chain;
        first = false;
    }
    out!("{} queued", f.store.queued_count()?);
    Ok(())
}

async fn supervise_now(id: i64) -> Result<()> {
    let f = Forge::open(true, false)?;
    match crate::supervisor::supervise(&f, id).await? {
        crate::supervisor::Ruled::Answered { retry } => out!("answered; re-queued as task {retry}"),
        crate::supervisor::Ruled::Prerequisite {
            prerequisite,
            retry,
        } => out!("filed prerequisite task {prerequisite}; re-queued as task {retry} behind it"),
        crate::supervisor::Ruled::Superseded { by } => out!("superseded by task {by}"),
        crate::supervisor::Ruled::Accepted { landed } => out!("accepted the branch: {landed}"),
        crate::supervisor::Ruled::Escalated(why) => out!("escalated: {why}"),
        crate::supervisor::Ruled::Skipped(why) => out!("skipped: {why}"),
    }
    Ok(())
}

fn decisions(
    repo: Option<PathBuf>,
    project: Option<String>,
    initiative: Option<i64>,
    json: bool,
) -> Result<()> {
    let f = Forge::open(false, false)?;
    let repo = repo
        .map(|p| p.canonicalize().context("repo path"))
        .transpose()?
        .map(|p| p.display().to_string());
    let rows: Vec<crate::view::DecisionRow> = f
        .store
        .decisions(&crate::store::DecisionFilter {
            repo,
            project,
            initiative,
        })?
        .iter()
        .map(|d| {
            let outcome = d
                .retry_id
                .and_then(|r| f.store.task(r).ok().flatten())
                .map(|t| t.state);
            crate::view::DecisionRow::new(d, outcome)
        })
        .collect();
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no decisions");
        return Ok(());
    }
    for d in &rows {
        let outcome = d
            .outcome
            .as_ref()
            .map(|s| format!(" → task {} {}", d.retry_id.unwrap_or_default(), s))
            .unwrap_or_default();
        out!("{:<5} task {:<5} Q: {}", d.id, d.task_id, d.question);
        out!(
            "{:<17}A ({}{}): {}{}",
            "",
            d.answered_by,
            if d.citations.is_empty() {
                String::new()
            } else {
                format!(", citing {}", d.citations)
            },
            d.answer,
            outcome
        );
    }
    Ok(())
}

fn ref_add(task: i64, kind: String, url: String, label: String, by: String) -> Result<()> {
    let f = Forge::open(false, false)?;
    if f.store.task(task)?.is_none() {
        bail!("no task {task}")
    }
    let id = f.store.insert_task_ref(task, &kind, &url, &label, &by)?;
    out!("{id} {kind} {url}");
    Ok(())
}

fn ref_list(task: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let rows: Vec<crate::view::RefRow> = f
        .store
        .task_refs(task)?
        .iter()
        .map(crate::view::RefRow::from)
        .collect();
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no references");
        return Ok(());
    }
    for r in &rows {
        out!(
            "{:<5} {:<8} {}{}",
            r.id,
            r.kind,
            r.url,
            if r.label.is_empty() {
                String::new()
            } else {
                format!("  {}", r.label)
            }
        );
    }
    Ok(())
}

fn print_project_row(r: &crate::view::ProjectRow) {
    out!("name       {}", r.name);
    out!("purpose    {}", r.purpose);
    out!("created_at {}", r.created_at);
    if r.repos.is_empty() {
        out!("repos      none");
    }
    for repo in &r.repos {
        match &repo.scope {
            Some(scope) => out!("repo       {} ({scope})", repo.repo),
            None => out!("repo       {}", repo.repo),
        }
    }
    out!(
        "tasks      queued={} running={} succeeded={} failed={} unverified={} blocked={} withdrawn={}",
        r.queued,
        r.running,
        r.succeeded,
        r.failed,
        r.unverified,
        r.blocked,
        r.withdrawn
    );
    out!("cost       ${:.2}", r.cost_usd);
    out!(
        "defaults   workflow={} per-task=${} per-initiative=${} supervisor={} per-lineage={} protected={}",
        r.workflow.as_deref().unwrap_or("-"),
        r.per_task_usd
            .map_or("-".to_string(), |v| format!("{v:.2}")),
        r.per_initiative_usd
            .map_or("-".to_string(), |v| format!("{v:.2}")),
        r.supervisor_model.as_deref().unwrap_or("-"),
        r.supervisor_per_lineage
            .map_or("-".to_string(), |v| v.to_string()),
        if r.protected.is_empty() {
            "-".to_string()
        } else {
            r.protected.join(", ")
        }
    );
    if !r.role_providers.is_empty() {
        out!(
            "roles      {}",
            r.role_providers
                .iter()
                .map(|(role, provider)| format!("{role}={provider}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

fn project_new(name: String, purpose: String, repos: Vec<String>) -> Result<()> {
    let f = Forge::open(false, false)?;
    if f.store.project(&name)?.is_some() {
        bail!("project {name} already exists");
    }
    f.store.create_project(&crate::store::Project {
        name: name.clone(),
        purpose,
        created_at: unix_now(),
        ..Default::default()
    })?;
    for r in repos {
        let (path, scope) = match r.split_once(':') {
            Some((p, s)) => (p, Some(s)),
            None => (r.as_str(), None),
        };
        let repo = Path::new(path)
            .canonicalize()
            .with_context(|| format!("--repo {path}"))?;
        let scope_json = scope
            .map(|s| serde_json::to_string(&s.split(',').collect::<Vec<_>>()))
            .transpose()?;
        f.store
            .register_repo(&name, &repo.display().to_string(), scope_json.as_deref())?;
    }
    out!("created project {name}");
    Ok(())
}

/// Parse `forge project set --role`'s `<role>=<provider>` pairs: the role
/// must be one of `config::ROLES`, and the provider must be configured.
fn parse_role_providers(
    f: &Forge,
    role: &[String],
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut out = std::collections::BTreeMap::new();
    for pair in role {
        let (role, provider) = pair
            .split_once('=')
            .with_context(|| format!("--role {pair:?}: expected <role>=<provider>"))?;
        if !config::ROLES.contains(&role) {
            bail!(
                "--role {pair:?}: unknown role {role:?}; expected one of {}",
                config::ROLES.join(", ")
            );
        }
        if !f.providers.contains_key(provider) {
            bail!("--role {pair:?}: unknown provider {provider:?}; see `forge providers`");
        }
        out.insert(role.to_string(), provider.to_string());
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn project_set(
    name: String,
    workflow: Option<String>,
    per_task_usd: Option<f64>,
    per_initiative_usd: Option<f64>,
    supervisor_model: Option<String>,
    supervisor_per_lineage: Option<u32>,
    protected: Vec<String>,
    role: Vec<String>,
) -> Result<()> {
    let f = Forge::open(false, false)?;
    let role_providers = parse_role_providers(&f, &role)?;
    let d = crate::store::ProjectDefaults {
        workflow,
        per_task_usd,
        per_initiative_usd,
        supervisor_model,
        supervisor_per_lineage: supervisor_per_lineage.map(|v| v as i64),
        protected: (!protected.is_empty()).then_some(protected),
        role_providers,
    };
    if !f.store.set_project_defaults(&name, &d)? {
        bail!("no project {name}");
    }
    out!("updated project {name}");
    Ok(())
}

fn print_backlog_item(it: &crate::store::BacklogItem) {
    out!(
        "{:<5} {} {}",
        it.id,
        if it.done_at.is_some() { "done" } else { "open" },
        it.text
    );
}

fn project_backlog(name: String, add: Option<String>, done: Option<i64>, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .project(&name)?
        .with_context(|| format!("no project {name}"))?;
    if let Some(text) = add {
        let id = f.store.add_backlog(&name, &text)?;
        out!("added backlog item {id}");
    }
    if let Some(id) = done {
        if !f.store.mark_backlog_done(&name, id)? {
            bail!("no open backlog item {id} in project {name}");
        }
        out!("marked backlog item {id} done");
    }
    let items = f.store.backlog(&name)?;
    if json {
        out!(
            "{}",
            serde_json::to_string_pretty(
                &items
                    .iter()
                    .map(|it| serde_json::json!({
                        "id": it.id,
                        "project": it.project,
                        "text": it.text,
                        "created_at": it.created_at,
                        "done_at": it.done_at,
                    }))
                    .collect::<Vec<_>>()
            )?
        );
        return Ok(());
    }
    if items.is_empty() {
        out!("no backlog items");
        return Ok(());
    }
    for it in &items {
        print_backlog_item(it);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn project_deploy_add(
    project: String,
    name: String,
    repo: PathBuf,
    scope: Option<String>,
    method: String,
    args: Vec<String>,
    check: Option<String>,
    on_landing: bool,
) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .project(&project)?
        .with_context(|| format!("no project {project}"))?;
    let repo = repo
        .canonicalize()
        .with_context(|| format!("--repo {}", repo.display()))?;
    let scope_json = scope
        .map(|s| serde_json::to_string(&s.split(',').collect::<Vec<_>>()))
        .transpose()?;
    let mut arg_map = std::collections::BTreeMap::new();
    for pair in &args {
        let (k, v) = pair
            .split_once('=')
            .with_context(|| format!("--arg {pair:?}: expected <key>=<value>"))?;
        arg_map.insert(k.to_string(), v.to_string());
    }
    let check = match check {
        Some(c) => c,
        None if method == "deploy-static" => String::new(),
        None => bail!("--check is required for method {method:?}"),
    };
    f.store.add_deploy_target(&crate::store::DeployTarget {
        project: project.clone(),
        name: name.clone(),
        repo: repo.display().to_string(),
        scope: scope_json,
        method,
        args: arg_map,
        check_cmd: check,
        on_landing,
    })?;
    out!("added deploy target {name} to project {project}");
    Ok(())
}

fn print_deploy_target_row(t: &crate::view::DeployTargetRow) {
    out!(
        "{:<12} repo={} method={}{} on_landing={}",
        t.name,
        t.repo,
        t.method,
        t.scope
            .as_ref()
            .map(|s| format!(" scope={s}"))
            .unwrap_or_default(),
        t.on_landing
    );
    out!("{:<12} check={}", "", t.check_cmd);
}

fn project_deploy_list(project: String, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .project(&project)?
        .with_context(|| format!("no project {project}"))?;
    let rows: Vec<crate::view::DeployTargetRow> = f
        .store
        .deploy_targets(&project)?
        .iter()
        .map(crate::view::DeployTargetRow::from)
        .collect();
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no deploy targets");
        return Ok(());
    }
    for r in &rows {
        print_deploy_target_row(r);
    }
    Ok(())
}

/// Run a deploy target now (see docs/DEPLOY.md, "When a deploy runs").
async fn deploy_run(
    project: Option<String>,
    name: Option<String>,
    sha: Option<String>,
) -> Result<()> {
    let (project, name) = match (project, name) {
        (Some(p), Some(n)) => (p, n),
        _ => bail!("usage: forge deploy <project> <name> [--sha <commit>]"),
    };
    let f = Forge::open(false, false)?;
    if !crate::deploy::run(&f, &project, &name, sha, None).await? {
        std::process::exit(1);
    }
    Ok(())
}

fn print_deploy_row(r: &crate::view::DeployRow) {
    let sha = &r.sha[..r.sha.len().min(8)];
    let status = match r.check_ok {
        Some(true) => "ok".to_string(),
        Some(false) => match &r.rolled_back_to {
            Some(to) => format!("FAILED, rolled back to {}", &to[..to.len().min(8)]),
            None => "FAILED".to_string(),
        },
        None => "running".to_string(),
    };
    out!("{:<5} {:<12} {sha} {status}", r.id, r.target);
    if !r.reason.is_empty() {
        out!("{:<19}{}", "", r.reason);
    }
}

fn deploy_log(project: String, name: Option<String>, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .project(&project)?
        .with_context(|| format!("no project {project}"))?;
    let rows: Vec<crate::view::DeployRow> = f
        .store
        .deploys(&project, name.as_deref())?
        .iter()
        .map(crate::view::DeployRow::from)
        .collect();
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no deploys");
        return Ok(());
    }
    for r in &rows {
        print_deploy_row(r);
    }
    Ok(())
}

fn project_list(json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let rows = crate::view::project_rows(&f)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no projects");
        return Ok(());
    }
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            out!();
        }
        print_project_row(r);
    }
    Ok(())
}

fn project_show(name: String, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let p = f
        .store
        .project(&name)?
        .with_context(|| format!("no project {name}"))?;
    let row = crate::view::project_row(&f, &p)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&row)?);
        return Ok(());
    }
    print_project_row(&row);
    Ok(())
}

fn print_initiative_row(r: &crate::view::InitiativeRow) {
    out!("id         {}", r.id);
    out!("project    {}", r.project);
    out!("outcome    {}", r.outcome);
    out!(
        "state      {}{}",
        r.state,
        r.held_rule
            .as_deref()
            .map(|rule| format!(" ({rule})"))
            .unwrap_or_default()
    );
    out!(
        "tasks      queued={} running={} succeeded={} failed={} unverified={} blocked={} withdrawn={}",
        r.queued,
        r.running,
        r.succeeded,
        r.failed,
        r.unverified,
        r.blocked,
        r.withdrawn
    );
    out!(
        "cost       ${:.2}{}",
        r.cost_usd,
        r.budget_usd
            .map(|b| format!(" of ${b:.2}"))
            .unwrap_or_default()
    );
    out!("stop-after {}", r.stop_after_same_rule);
    if let Some(at) = r.settled_at {
        out!("settled_at {at}");
    }
}

async fn initiative_new(
    project: String,
    outcome: String,
    from: Option<PathBuf>,
    provider: Option<String>,
    budget: Option<f64>,
    stop_after: Option<u32>,
) -> Result<()> {
    if let Some(b) = budget
        && b <= 0.0
    {
        bail!("budget must be positive");
    }
    let f = Forge::open(false, false)?;
    f.store
        .project(&project)?
        .with_context(|| format!("no project {project}"))?;
    let id = f.store.create_initiative(&crate::store::Initiative {
        project: project.clone(),
        outcome,
        budget_usd: budget,
        stop_after_same_rule: stop_after.map(|n| n as i64).unwrap_or(3),
        created_at: unix_now(),
        ..Default::default()
    })?;
    out!("created initiative {id}");
    if let Some(path) = from {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let default_repo = f.store.first_repo(&project)?;
        let paragraphs = crate::queue::parse_initiative_file(&text)?;
        let known_provider = |name: &str| -> Result<()> {
            f.providers.get(name).with_context(|| {
                format!("unknown provider {name:?}; see `forge providers` for what is configured")
            })?;
            Ok(())
        };
        if let Some(p) = &provider {
            known_provider(p)?;
        }
        for p in &paragraphs {
            if let Some(pr) = &p.provider {
                known_provider(pr)?;
            }
        }
        let mut ids: Vec<i64> = Vec::new();
        for p in &paragraphs {
            let repo = match &p.repo {
                Some(r) => r.clone(),
                None => default_repo
                    .clone()
                    .with_context(|| format!("project {project} lists no repository"))?,
            };
            let after = match p.after {
                Some(n) => vec![
                    *ids.get(n - 1)
                        .with_context(|| format!("after: {n} names a task not yet queued"))?,
                ],
                None => Vec::new(),
            };
            let req = crate::queue::TaskRequest {
                repo: PathBuf::from(repo),
                task: p.text.clone(),
                provider: p.provider.clone().or_else(|| provider.clone()),
                max_turns: 100,
                retries: 1,
                timeout_secs: 1800,
                after,
                project: Some(project.clone()),
                initiative: Some(id),
                ..Default::default()
            };
            let t = crate::queue::enqueue(&f, &req, None).await?;
            out!("queued task {} (paragraph {})", t.id, ids.len() + 1);
            ids.push(t.id);
        }
    }
    Ok(())
}

async fn initiative_from_plan(task: i64, outcome: Option<String>) -> Result<()> {
    let f = Forge::open(false, false)?;
    let t = f
        .store
        .task(task)?
        .with_context(|| format!("no task {task}"))?;
    if t.plan.is_empty() {
        bail!(
            "task {task} has no recorded plan (the investigate directive did not run, or found none)"
        );
    }
    let project = t
        .project
        .clone()
        .with_context(|| format!("task {task} has no project"))?;
    let id = f.store.create_initiative(&crate::store::Initiative {
        project,
        outcome: outcome.unwrap_or_else(|| t.task.clone()),
        stop_after_same_rule: 3,
        created_at: unix_now(),
        ..Default::default()
    })?;
    out!("created initiative {id}");
    let ids = crate::queue::file_plan(&f, &t, id).await?;
    for (n, tid) in ids.iter().enumerate() {
        out!("queued task {tid} (plan item {})", n + 1);
    }
    Ok(())
}

fn initiative_list(project: Option<String>, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let rows = crate::view::initiative_rows(&f, project.as_deref())?;
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no initiatives");
        return Ok(());
    }
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            out!();
        }
        print_initiative_row(r);
    }
    Ok(())
}

fn initiative_show(id: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let ini = f
        .store
        .initiative(id)?
        .with_context(|| format!("no initiative {id}"))?;
    let row = crate::view::initiative_row(&f, &ini)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&row)?);
        return Ok(());
    }
    print_initiative_row(&row);
    Ok(())
}

fn initiative_report(id: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let ini = f
        .store
        .initiative(id)?
        .with_context(|| format!("no initiative {id}"))?;
    let doc = crate::view::initiative_doc(&f, &ini)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    out!("initiative {} ({})", doc.id, doc.project);
    out!("outcome    {}", doc.outcome);
    out!(
        "state      {}{}",
        doc.state,
        doc.held_rule
            .as_deref()
            .map(|rule| format!(" ({rule})"))
            .unwrap_or_default()
    );
    out!(
        "cost       ${:.2}{}",
        doc.cost_usd,
        doc.budget_usd
            .map(|b| format!(" of ${b:.2}"))
            .unwrap_or_default()
    );
    out!(
        "elapsed    {}",
        doc.elapsed_secs
            .map_or("-".to_string(), |s| format!("{s}s"))
    );
    out!("tasks");
    for t in &doc.tasks {
        out!(
            "  {:<5} {:<10}{}{}",
            t.id,
            t.state,
            match t.retries {
                0 => String::new(),
                1 => " (1 retry)".to_string(),
                n => format!(" ({n} retries)"),
            },
            if t.reason.is_empty() {
                String::new()
            } else {
                format!(" {}", t.reason)
            }
        );
    }
    if doc.refused.is_empty() {
        out!("refused    none");
    } else {
        out!(
            "refused    {}",
            doc.refused
                .iter()
                .map(|r| format!("{} x{}", r.rule, r.count))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if doc.rulings.is_empty() {
        out!("rulings    none");
    } else {
        out!("rulings");
        for r in &doc.rulings {
            out!("  task {} Q: {} A: {}", r.task_id, r.question, r.answer);
        }
    }
    if doc.questions.is_empty() {
        out!("questions  none");
    } else {
        out!("questions");
        for q in &doc.questions {
            out!(
                "  task {} {}{}",
                q.task_id,
                q.question,
                q.answer
                    .as_deref()
                    .map(|a| format!(" -> {a}"))
                    .unwrap_or_else(|| " (unanswered)".to_string())
            );
        }
    }
    for d in &doc.deployed {
        let sha = &d.sha[..d.sha.len().min(8)];
        let status = match d.check_ok {
            Some(true) => "ok".to_string(),
            Some(false) => match &d.rolled_back_to {
                Some(to) => format!("rolled back to {}", &to[..to.len().min(8)]),
                None => "failed".to_string(),
            },
            None => "running".to_string(),
        };
        out!(
            "deployed   task {} {} @ {sha} {status}",
            d.task_id,
            d.target
        );
    }
    Ok(())
}

async fn add(args: TaskArgs) -> Result<()> {
    let f = Forge::open(false, false)?;
    let t = enqueue(&f, &args).await?;
    out!("queued task {} ({} queued)", t.id, f.store.queued_count()?);
    Ok(())
}

fn version() -> Result<()> {
    let sha = env!("FORGE_GIT_SHA");
    if sha.is_empty() {
        out!("{}", env!("CARGO_PKG_VERSION"));
    } else {
        out!("{} ({})", env!("CARGO_PKG_VERSION"), sha);
    }
    Ok(())
}

fn run_doctor(json: bool) -> Result<()> {
    let checks = doctor::run()?;
    let failed = checks.iter().any(|c| c.status == doctor::Status::Fail);
    if json {
        out!("{}", serde_json::to_string(&checks)?);
        if failed {
            std::process::exit(1);
        }
        return Ok(());
    }
    for c in &checks {
        let tag = match c.status {
            doctor::Status::Ok => "OK  ",
            doctor::Status::Warn => "WARN",
            doctor::Status::Fail => "FAIL",
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

fn measure(f: &Forge, w: &workflows::Workflow) -> Result<profile::Measured> {
    profile::measure(&f.store, &w.name, &w.hash)
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
                if a.contract.as_str() != a.name {
                    format!("contract {}", a.contract)
                } else {
                    String::new()
                }
            }
        };
        let flow = match (a.consumes.is_empty(), a.produces.is_empty()) {
            (true, true) => String::new(),
            _ => format!(
                "  {} → {}",
                a.consumes
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                a.produces
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
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

fn list_providers(json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    if json {
        let docs: Vec<serde_json::Value> = f
            .providers
            .values()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "runner": p.runner.as_str(),
                    "model": p.model,
                    "base_url": p.base_url,
                    "env": p.env.iter().map(|(k, _)| k).collect::<Vec<_>>(),
                    "extra_args": p.extra_args,
                    "notes": p.notes,
                })
            })
            .collect();
        out!("{}", serde_json::to_string_pretty(&docs)?);
        return Ok(());
    }
    for p in f.providers.values() {
        out!(
            "{:<12} {:<10} {}",
            p.name,
            p.runner.as_str(),
            p.model.as_deref().unwrap_or("(runner default)"),
        );
        if let Some(n) = &p.notes {
            out!("             {n}");
        }
    }
    Ok(())
}

fn plugin_list(json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let (rows, problems) = crate::view::plugin_rows(&f)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let home_cfg = config::load_home(&f.paths.home)?;
    let cat = crate::plugins::load_catalog(&f.paths.home, &home_cfg.plugin_dirs);
    for r in &rows {
        out!(
            "{:<16} {:<8} {:<10} {:<16} {}",
            r.name,
            if r.enabled { "enabled" } else { "disabled" },
            r.restart,
            r.capabilities.join(","),
            r.dir,
        );
        if !r.description.is_empty() {
            out!("             {}", r.description);
        }
        if let Some(p) = cat.plugins.get(&r.name) {
            out!("             runs       {}", p.manifest.run.join(" "));
            if let Some(b) = &p.manifest.build {
                out!("             build      {}", b.join(" "));
            }
        }
    }
    for p in &problems {
        out!("problem: {} {}", p.file, p.what);
    }
    Ok(())
}

fn plugin_status(name: Option<String>, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let (rows, _) = crate::view::plugin_rows(&f)?;
    let statuses: Vec<crate::view::PluginStatusRow> = rows
        .iter()
        .filter(|r| name.as_deref().map(|n| n == r.name).unwrap_or(true))
        .map(|r| {
            let run_state = crate::plugins::read_run_state(&f.paths.home, &r.name);
            crate::view::PluginStatusRow::new(r.name.clone(), r.enabled, &run_state)
        })
        .collect();
    if let Some(n) = &name
        && statuses.is_empty()
    {
        bail!("no such plugin: {n:?}");
    }
    if json {
        if name.is_some() {
            out!("{}", serde_json::to_string_pretty(&statuses[0])?);
        } else {
            out!("{}", serde_json::to_string_pretty(&statuses)?);
        }
        return Ok(());
    }
    for s in &statuses {
        out!(
            "{:<16} {}  {}",
            s.name,
            if s.enabled { "enabled" } else { "disabled" },
            match s.state.as_str() {
                "running" => format!(
                    "running pid {}, up {}s",
                    s.pid.unwrap_or(0),
                    s.uptime_secs.unwrap_or(0)
                ),
                "restarting" => format!("restarting (x{})", s.restart_count.unwrap_or(0)),
                _ => match &s.last_exit {
                    Some(e) => format!("stopped: {e}"),
                    None => "stopped".to_string(),
                },
            }
        );
    }
    Ok(())
}

fn plugin_set_enabled(name: String, enabled: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let home_cfg = config::load_home(&f.paths.home)?;
    let cat = crate::plugins::load_catalog(&f.paths.home, &home_cfg.plugin_dirs);
    if !cat.plugins.contains_key(&name) {
        bail!("no such plugin: {name:?}");
    }
    f.store.set_plugin_enabled(&name, enabled, unix_now())?;
    out!("{name} {}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}

fn plugin_install(path: PathBuf) -> Result<()> {
    let f = Forge::open(false, false)?;
    let manifest = crate::plugins::install(&f.paths.home, &path)?;
    out!(
        "installed {} at {}",
        manifest.name,
        f.paths.home.join("plugins").join(&manifest.name).display()
    );
    if let Some(build) = &manifest.build {
        out!("build {} ok", build.join(" "));
    }
    Ok(())
}

fn plugin_uninstall(name: String) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store.set_plugin_enabled(&name, false, unix_now())?;
    crate::plugins::remove_installed(&f.paths.home, &name)?;
    out!("uninstalled {name}; left plugins-state/{name} alone");
    Ok(())
}

fn plugin_logs(name: String, follow: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let path = f
        .paths
        .home
        .join("logs")
        .join("plugins")
        .join(format!("{name}.log"));
    use std::io::Read;
    let mut file = std::fs::File::open(&path)
        .with_context(|| format!("no log yet for plugin {name:?} ({})", path.display()))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    std::io::stdout().write_all(&buf)?;
    std::io::stdout().flush()?;
    if follow {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let mut more = Vec::new();
            file.read_to_end(&mut more)?;
            if !more.is_empty() {
                std::io::stdout().write_all(&more)?;
                std::io::stdout().flush()?;
            }
        }
    }
    Ok(())
}

fn trace(id: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let Some(t) = f.store.task(id)? else {
        bail!("no task {id}")
    };
    let doc = crate::view::trace_doc(&f, &t)?;
    if json {
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
    if let Ok(r) = serde_json::from_value::<workflows::Resolved>(doc.resolved.clone()) {
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
    for o in doc.ops.iter().filter(|o| o.attempt_id.is_none()) {
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
    for a in &doc.attempts {
        out!();
        out!(
            "=== attempt {} [{} seq {}] {}{}",
            a.attempt_no,
            a.step,
            a.step_seq,
            a.state,
            if a.reason.is_empty() {
                String::new()
            } else {
                format!(": {}", a.reason)
            }
        );
        let inputs: audit::Inputs = serde_json::from_value(a.inputs.clone()).unwrap_or_default();
        out!(
            "inputs     model={} runner={} provider={} max_turns={} timeout={}s base={} start={}",
            inputs.model,
            a.runner,
            a.provider,
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
        if let Ok(rows) =
            serde_json::from_value::<Vec<crate::checks::CheckResult>>(a.verdict.clone())
        {
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
        let outputs: audit::Outputs = serde_json::from_value(a.outputs.clone()).unwrap_or_default();
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
    for dgn in &doc.diagnosis {
        out!();
        out!("what       {}", dgn.what);
        out!("action     {}", dgn.action);
    }
    Ok(())
}

fn requests(repo: Option<PathBuf>, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let repo = repo
        .map(|p| p.canonicalize().context("repo path"))
        .transpose()?
        .map(|p| p.display().to_string());
    let rows = requests_json(&f, repo.as_deref())?;
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
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
    for r in &rows {
        let repo_name = Path::new(&r.repo)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out!(
            "{:<5} {:<9} {:<8} {:<18} {}",
            r.id,
            r.kind,
            r.workflow,
            repo_name,
            r.text
        );
        if !r.tried.is_empty() {
            out!("{:<44} did: {}", "", r.tried);
        }
    }
    Ok(())
}

fn collect_tool_stats(
    f: &Forge,
    step: Option<&str>,
) -> Result<std::collections::BTreeMap<String, (usize, crate::tools::Tools)>> {
    use std::collections::BTreeMap;
    // step -> aggregated tools
    let mut per_step: BTreeMap<String, (usize, crate::tools::Tools)> = BTreeMap::new();
    for (_task_id, step, outputs_json) in f.store.attempt_tool_facts(step)? {
        let Ok(o) = serde_json::from_str::<audit::Outputs>(&outputs_json) else {
            continue;
        };
        let Some(tools) = o.tools else {
            continue;
        };
        let e = per_step
            .entry(step)
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
    Ok(per_step)
}

fn tool_stats(f: &Forge, step: Option<&str>) -> Result<()> {
    let per_step = collect_tool_stats(f, step)?;
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

fn tools_json(f: &Forge, step: Option<&str>) -> Result<serde_json::Value> {
    let per_step = collect_tool_stats(f, step)?;
    let mut steps = serde_json::Map::new();
    for (step, (n, t)) in per_step {
        let by_tool: serde_json::Map<String, serde_json::Value> = t
            .by_tool
            .iter()
            .map(|(name, u)| {
                let s = u.ms as f64 / 1000.0;
                (
                    name.clone(),
                    serde_json::json!({
                        "calls": u.calls,
                        "total_s": s,
                        "s_per_call": if u.calls > 0 { s / u.calls as f64 } else { 0.0 },
                    }),
                )
            })
            .collect();
        let shell: serde_json::Map<String, serde_json::Value> = t
            .shell
            .iter()
            .map(|(name, u)| {
                let s = u.ms as f64 / 1000.0;
                (
                    name.clone(),
                    serde_json::json!({
                        "calls": u.calls,
                        "total_s": s,
                        "s_per_call": if u.calls > 0 { s / u.calls as f64 } else { 0.0 },
                    }),
                )
            })
            .collect();
        steps.insert(
            step,
            serde_json::json!({
                "attempts": n,
                "by_tool": by_tool,
                "shell": shell,
                "reads": t.reads,
            }),
        );
    }
    Ok(serde_json::Value::Object(steps))
}

#[allow(clippy::too_many_arguments)]
fn stats(
    tools: bool,
    step: Option<String>,
    quality: bool,
    journal: bool,
    by_role: bool,
    project: Option<String>,
    initiative: Option<i64>,
    json: bool,
) -> Result<()> {
    let f = Forge::open(false, false)?;
    let scope = crate::store::StatsFilter {
        project,
        initiative,
    };
    if json {
        let mut doc = crate::view::stats_doc(&f, &scope)?;
        if tools {
            doc.tools = Some(tools_json(&f, step.as_deref())?);
        }
        out!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    if tools {
        return tool_stats(&f, step.as_deref());
    }
    if quality {
        return quality_stats(&f, &scope);
    }
    if journal {
        return journal_control_stats(&f);
    }
    if by_role {
        return by_role_stats(&f);
    }
    let doc = crate::view::stats_doc(&f, &scope)?;
    out!(
        "{:<8} {:<16} {:>5} {:>4} {:>4} {:>4} {:>4} {:>5} {:>9} {:>9} {:>6} {:>9}",
        "WF",
        "HASH",
        "TASKS",
        "OK",
        "FAIL",
        "BLK",
        "UNV",
        "ATT",
        "COST",
        "$/OK",
        "LANDED",
        "$/LANDED"
    );
    for w in &doc.workflows {
        out!(
            "{:<8} {:<16} {:>5} {:>4} {:>4} {:>4} {:>4} {:>5} {:>9} {:>9} {:>6} {:>9}",
            w.workflow,
            w.hash,
            w.pieces,
            w.succeeded,
            w.failed,
            w.blocked,
            w.unverified,
            w.attempts,
            format!("${:.2}", w.mean_cost_usd),
            match w.cost_per_success_usd {
                Some(c) => format!("${c:.2}"),
                None => "-".into(),
            },
            w.landed,
            match w.cost_per_landed_usd {
                Some(c) => format!("${c:.2}"),
                None => "-".into(),
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
    for st in &doc.steps {
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
            st.mean_secs,
            format!("${:.2}", st.cost_usd),
            st.mean_input_tokens
                .map_or("-".to_string(), |v| format!("{v:.0}"))
        );
    }
    if !doc.projects.is_empty() {
        out!();
        out!(
            "{:<16} {:>5} {:>6} {:>9} {:>7}",
            "PROJECT",
            "TASKS",
            "LANDED",
            "COST",
            "DEFECT%"
        );
        for p in &doc.projects {
            out!(
                "{:<16} {:>5} {:>6} {:>9} {:>7}",
                p.project,
                p.tasks,
                p.landed,
                format!("${:.2}", p.cost_usd),
                match p.broke_base_share {
                    Some(s) => format!("{:.0}%", s * 100.0),
                    None => "-".into(),
                }
            );
        }
    }
    Ok(())
}

/// Defect escape, per workflow: of the tasks that landed, how many broke
/// the next task's base or were later repaired.
fn quality_stats(f: &Forge, scope: &crate::store::StatsFilter) -> Result<()> {
    let doc = crate::view::stats_doc(f, scope)?;
    out!(
        "{:<8} {:<16} {:>6} {:>10} {:>9} {:>8} {:>9}",
        "WF",
        "HASH",
        "LANDED",
        "BROKEBASE",
        "BROKE%",
        "REPAIRED",
        "REPAIR%"
    );
    let pct = |share: Option<f64>| match share {
        Some(s) => format!("{:.0}%", s * 100.0),
        None => "-".into(),
    };
    for w in &doc.workflows {
        out!(
            "{:<8} {:<16} {:>6} {:>10} {:>9} {:>8} {:>9}",
            w.workflow,
            w.hash,
            w.landed,
            w.broke_base,
            pct(w.broke_base_share),
            w.repaired,
            pct(w.repaired_share)
        );
    }
    Ok(())
}

/// Attempts, outcomes, cost and wall time per (role, provider, model),
/// role being the attempt's step; landed and broke-base counts for the
/// `code` role only.
fn by_role_stats(f: &Forge) -> Result<()> {
    let doc = crate::view::stats_doc(f, &crate::store::StatsFilter::default())?;
    out!(
        "{:<10} {:<10} {:<16} {:>5} {:>8} {:>6} {:>9} {:>7} {:>6} {:>9} {:>7}",
        "ROLE",
        "PROVIDER",
        "MODEL",
        "ATT",
        "SUCCEED%",
        "TURNS",
        "COST",
        "SECS",
        "LANDED",
        "BROKEBASE",
        "BROKE%"
    );
    let pct = |share: Option<f64>| match share {
        Some(s) => format!("{:.0}%", s * 100.0),
        None => "-".into(),
    };
    let count = |v: Option<i64>| v.map_or("-".to_string(), |n| n.to_string());
    for r in &doc.by_role {
        out!(
            "{:<10} {:<10} {:<16} {:>5} {:>8} {:>6.1} {:>9} {:>7.0} {:>6} {:>9} {:>7}",
            r.role,
            r.provider,
            r.model,
            r.attempts,
            pct(r.succeeded_share),
            r.mean_turns,
            format!("${:.2}", r.mean_cost_usd),
            r.mean_secs,
            count(r.landed),
            count(r.broke_base),
            pct(r.broke_base_share)
        );
    }
    Ok(())
}

/// The journal control arm's retrospective split: code attempts after the
/// first (`attempt_no > 1`), by whether they were handed a journal. See
/// docs/LATER.md, "The journal measurement was ill-posed three times".
fn journal_control_stats(f: &Forge) -> Result<()> {
    let doc = crate::view::stats_doc(f, &crate::store::StatsFilter::default())?;
    out!(
        "{:<11} {:>5} {:>6} {:>7} {:>10} {:>9}",
        "ARM",
        "ATT",
        "TURNS",
        "EDIT@",
        "SUCCEED%",
        "COST"
    );
    let pct = |share: Option<f64>| match share {
        Some(s) => format!("{:.0}%", s * 100.0),
        None => "-".into(),
    };
    for (arm, row) in [("journal", &doc.journal), ("no journal", &doc.no_journal)] {
        out!(
            "{:<11} {:>5} {:>6.1} {:>7} {:>10} {:>9}",
            arm,
            row.attempts,
            row.mean_turns,
            row.mean_first_edit
                .map_or("-".to_string(), |v| format!("{v:.1}")),
            pct(row.succeeded_share),
            format!("${:.2}", row.mean_cost_usd)
        );
    }
    Ok(())
}

fn tasks_json(f: &Forge, q: &crate::store::TaskFilter) -> Result<Vec<crate::view::TaskRow>> {
    Ok(f.store
        .list_tasks_where(q)?
        .iter()
        .map(crate::view::TaskRow::from)
        .collect())
}

fn requests_json(f: &Forge, repo: Option<&str>) -> Result<Vec<crate::view::RequestRow>> {
    Ok(f.store
        .blocked(repo)?
        .iter()
        .map(|t| {
            let q = f
                .store
                .attempts(t.id)
                .ok()
                .and_then(|a| {
                    a.last().and_then(|a| {
                        serde_json::from_str::<crate::envelope::Envelope>(&a.envelope_json).ok()
                    })
                })
                .and_then(|e| e.needs_input);
            crate::view::RequestRow {
                tried: q.as_ref().map(|q| q.tried.clone()).unwrap_or_default(),
                path: q.as_ref().map(|q| q.path.clone()).unwrap_or_default(),
                ..crate::view::RequestRow::from(t)
            }
        })
        .collect())
}

/// The worker as the pid file says: pid, its binary, whether it is alive,
/// and whether that binary was rebuilt underneath it.
fn worker_json(f: &Forge) -> serde_json::Value {
    match worker::worker_status(&f.paths) {
        None => serde_json::json!({"running": false}),
        Some(w) => {
            serde_json::json!({"running": w.running, "pid": w.pid, "exe": w.exe, "stale_binary": w.stale})
        }
    }
}

fn journal(id: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let Some(t) = f.store.task(id)? else {
        bail!("no task {id}");
    };
    if json {
        let entries = crate::journal::entries_for(&f, &t).map_err(|e| match e {
            crate::engine::Fault::Task(e) | crate::engine::Fault::Env(e) => e,
        })?;
        out!("{}", serde_json::to_string(&entries)?);
        return Ok(());
    }
    let j = crate::journal::journal_for(&f, &t).map_err(|e| match e {
        crate::engine::Fault::Task(e) | crate::engine::Fault::Env(e) => e,
    })?;
    if j.is_empty() {
        out!("nothing ran before task {id} in its piece of work");
    } else {
        out!("{j}");
    }
    Ok(())
}

/// The integrator, by hand, for a task that verified but did not land: a
/// task from before the repository had a remote, or one queued --no-land
/// that a human has now cleared.
async fn land(id: i64) -> Result<()> {
    let f = Forge::open(true, true)?;
    let line = land_task(&f, id).await?;
    out!("{line}");
    Ok(())
}

/// Whether a blocked task's last attempt, though it did not settle,
/// still leaves a branch worth landing: a review demotion the operator
/// or the supervisor set aside, naming no defect the task requires
/// fixing, or a question whose L1 checks already ran on a clean,
/// committed tree and all passed. The question or the demotion stands
/// either way; neither says the commit itself is bad.
pub(crate) fn landable_needs_input(f: &Forge, t: &Task) -> Result<bool> {
    if t.state != TaskState::Blocked {
        return Ok(false);
    }
    Ok(f.store
        .attempts(t.id)?
        .iter()
        .rev()
        .find(|a| a.is_agent())
        .is_some_and(|a| {
            a.state == crate::store::AttemptState::NeedsInput
                && (a.reason.starts_with("review demoted")
                    || crate::verify::l1_all_passed(
                        &serde_json::from_str::<Vec<crate::checks::CheckResult>>(&a.verdict_json)
                            .unwrap_or_default(),
                    ))
        }))
}

/// Land a task's verified branch on the base: a verified task, or one
/// blocked on a review demotion or a question that a human or the
/// supervisor set aside (see `landable_needs_input`). Returns the line
/// to print.
pub(crate) async fn land_task(f: &Forge, id: i64) -> Result<String> {
    let Some(mut t) = f.store.task(id)? else {
        bail!("no task {id}");
    };
    let demoted = landable_needs_input(f, &t)?;
    if t.state != TaskState::Succeeded && t.state != TaskState::Unverified && !demoted {
        bail!(
            "task {id} is {}; only a verified task lands",
            t.state.as_str()
        );
    }
    if !t.landed_sha.is_empty() {
        bail!("task {id} already landed: {}", t.reason);
    }
    let repo = PathBuf::from(&t.repo);
    if !Path::new(&t.worktree).join(".git").exists() {
        bail!(
            "task {id}'s worktree is gone ({}); retry the task instead",
            t.worktree
        );
    }
    let cfg = config::load_working(&repo).await?;
    let Some(remote) = cfg.push_remote.clone() else {
        bail!("{} has no push remote; nothing to land on", repo.display());
    };
    let Some(url) = git::remote_url(&repo, &remote).await else {
        bail!("remote {remote} has no URL in {}", repo.display());
    };
    let mut seq = f.store.ops(id)?.len() as i64;
    let mut attempt_no = f.store.attempts(id)?.len() as i64;
    match crate::landing::integrate(f, &mut t, &url, &remote, &mut seq, &mut attempt_no)
        .await
        .map_err(|e| match e {
            crate::engine::Fault::Task(e) | crate::engine::Fault::Env(e) => e,
        })? {
        crate::landing::Integrate::Landed(sha) => {
            t.reason = format!("landed {} @ {}", t.base_branch, &sha[..sha.len().min(8)]);
            t.landed_sha = sha.clone();
            t.pushed = true;
            if demoted {
                t.state = TaskState::Succeeded;
                t.finished_at = Some(crate::unix_now());
            }
            f.store.update_task(&t)?;
            if let Some(iid) = t.initiative {
                crate::view::maybe_settle_initiative(f, id, iid)?;
            }
            f.report.emit(
                id,
                crate::report::Event::TaskDone {
                    state: t.state.as_str(),
                    attempts: f.store.attempts(id)?.len(),
                    cost: f.store.task_cost(id)?,
                    reason: &t.reason,
                    branch: &t.branch,
                    pushed: true,
                    compare: None,
                    remove_cmd: "",
                },
            );
            Ok(format!(
                "landed task {id} on {} @ {}",
                t.base_branch,
                &sha[..8]
            ))
        }
        crate::landing::Integrate::Rewind { first, .. } => {
            // No need to store base_sha or reload cfg here: this command
            // only reports the conflict and exits without touching `t` or
            // `cfg` again. `forge retry` enqueues a brand-new task rather
            // than resuming this one, so the stale base_sha left on this
            // task is never read.
            bail!(
                "task {id} needs the coder again: {first}\n  forge retry {id} runs it through the integrator with the conflict as feedback"
            )
        }
        crate::landing::Integrate::Failed(reason) => bail!("task {id} could not land: {reason}"),
    }
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
            scratch: None,
            report_from_git: false,
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
        "tasks": tasks_json(&f, &crate::store::TaskFilter { limit: 200, ..Default::default() })?,
        "requests": requests_json(&f, None)?,
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

/// `forge log`'s filters, gathered into one struct so the function that
/// applies them stays under clippy's argument-count lint.
struct LogArgs {
    limit: u32,
    state: Option<String>,
    repo: Option<PathBuf>,
    before: Option<i64>,
    grep: Option<String>,
    workflow: Option<String>,
    project: Option<String>,
    initiative: Option<i64>,
}

fn log(args: LogArgs, json: bool) -> Result<()> {
    let LogArgs {
        limit,
        state,
        repo,
        before,
        grep,
        workflow,
        project,
        initiative,
    } = args;
    let state = state
        .map(|s| {
            TaskState::try_from(s.as_str()).map_err(|_| {
                anyhow::anyhow!(
                    "unknown state {s:?}; valid states are queued, running, succeeded, failed, blocked, unverified"
                )
            })
        })
        .transpose()?;
    let repo = repo
        .map(|p| p.canonicalize().context("repo path"))
        .transpose()?
        .map(|p| p.display().to_string());
    let f = Forge::open(false, false)?;
    let q = crate::store::TaskFilter {
        limit,
        state,
        repo,
        before,
        grep,
        workflow,
        project,
        initiative,
    };
    let rows = tasks_json(&f, &q)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
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
    for s in &rows {
        let repo_name = Path::new(&s.repo)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.repo.clone());
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
            format!("${:.4}", s.cost_usd),
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
    let doc = crate::view::trace_doc(&f, &t)?;
    let task = &doc.task;
    let cost: f64 = doc.attempts.iter().filter_map(|a| a.cost_usd).sum();
    out!("task       {}", task.id);
    out!(
        "state      {}{}",
        task.state,
        if task.reason.is_empty() {
            String::new()
        } else {
            format!(" ({})", task.reason)
        }
    );
    out!("repo       {}", task.repo);
    if let Some(pname) = &t.project {
        out!("project    {pname}");
    }
    let effective_supervisor = f.effective_supervisor(&t);
    let scope = f.effective_paths(&t);
    out!(
        "defaults   per-task cap ${:.2}, supervisor {} (per-lineage {}){}",
        f.effective_per_task_usd(&t),
        effective_supervisor.model,
        effective_supervisor.per_lineage,
        if scope.is_empty() {
            String::new()
        } else {
            format!(", scope {}", scope.join(", "))
        }
    );
    out!(
        "base       {} @ {}",
        task.base_branch,
        if task.base_sha.is_empty() {
            "-"
        } else {
            &task.base_sha[..8]
        }
    );
    out!(
        "branch     {}{}",
        if task.branch.is_empty() {
            "-"
        } else {
            &task.branch
        },
        if task.pushed { " (pushed)" } else { "" }
    );
    out!(
        "worktree   {}{}",
        if task.worktree.is_empty() {
            "-"
        } else {
            &task.worktree
        },
        if task.worktree_removed_at.is_some() {
            " (removed)"
        } else {
            ""
        }
    );
    out!(
        "model      {} (max {} turns, max {} attempts, {}s timeout)",
        task.model,
        task.max_turns,
        task.max_attempts,
        task.timeout_secs
    );
    out!(
        "provider   {}",
        if task.provider.is_empty() {
            "(per-role; see forge providers)"
        } else {
            &task.provider
        }
    );
    out!(
        "cost       ${cost:.4} over {} attempt(s){}",
        doc.attempts.len(),
        task.budget_usd
            .map_or(String::new(), |b| format!(" (task cap ${b:.2})"))
    );
    for c in &task.checks {
        out!("check      $ {c}");
    }
    if task.allow_protected {
        out!("protected  changes allowed");
    }
    if !task.land {
        out!("land       manual: the verified branch is left for a human");
    }
    out!(
        "journal    {} ({})",
        if task.journal_enabled { "on" } else { "off" },
        task.journal_arm
    );
    if !task.after.is_empty() {
        out!(
            "after      {}",
            task.after
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(r) = task.retry_of {
        out!("retry of   {r}");
    }
    if task.lineage.len() > 1 {
        out!(
            "lineage    {}",
            task.lineage
                .iter()
                .map(|l| if l.id == task.id {
                    format!("[{} {}]", l.id, l.state)
                } else {
                    format!("{} {}", l.id, l.state)
                })
                .collect::<Vec<_>>()
                .join(" → ")
        );
    }
    for d in &doc.deploys {
        let sha = &d.sha[..d.sha.len().min(8)];
        let status = match d.check_ok {
            Some(true) => "ok".to_string(),
            Some(false) => match &d.rolled_back_to {
                Some(to) => format!("rolled back to {}", &to[..to.len().min(8)]),
                None => "failed".to_string(),
            },
            None => "running".to_string(),
        };
        out!(
            "{:<11}{} {sha} {status} {}",
            "deploy",
            d.target,
            d.finished_at.unwrap_or(d.started_at)
        );
    }
    for r in &task.refs {
        let label = if r.label.is_empty() {
            String::new()
        } else {
            format!(" ({})", r.label)
        };
        out!("{:<11}{} {}{}", "ref", r.kind, r.url, label);
    }
    for d in &task.decisions {
        out!("decision   {} → {}", d.question, d.answer);
    }
    out!("workflow   {} {}", task.workflow, task.workflow_hash);
    if !task.interface.is_empty() {
        out!(
            "interface  {}",
            task.interface.lines().collect::<Vec<_>>().join(" / ")
        );
    }
    if !task.plan.is_empty() {
        out!(
            "plan       {}",
            task.plan.lines().collect::<Vec<_>>().join(" / ")
        );
    }
    out!("text       {}", task.text);
    for a in &doc.attempts {
        out!();
        out!(
            "attempt {} [{}]  {}{}  {}  {} turns  {} tools  {:.1}s  {}  {} commit(s)  {} file(s){}",
            a.attempt_no,
            a.step,
            a.state,
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
        out!("  agent   runner={} provider={}", a.runner, a.provider);
        if let Ok(o) = serde_json::from_value::<audit::Outputs>(a.outputs.clone())
            && let Some(t) = o.tools
        {
            out!("  ran     {}", t.line());
        }
        if let Ok(checks) =
            serde_json::from_value::<Vec<crate::checks::CheckResult>>(a.verdict.clone())
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
        let envelope: Option<crate::envelope::Envelope> = if a.envelope.is_null() {
            None
        } else {
            serde_json::from_value(a.envelope.clone()).ok()
        };
        if let Some(e) = envelope {
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
        if a.rate_limits.five_hour.is_some() || a.rate_limits.seven_day.is_some() {
            out!(
                "  usage   5h {} · 7d {}",
                a.rate_limits
                    .five_hour
                    .map_or("-".into(), |u| format!("{:.0}%", u * 100.0)),
                a.rate_limits
                    .seven_day
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
    for dgn in &doc.diagnosis {
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
                let _ = std::fs::remove_dir_all(crate::attempt::tests_clone_dir(&t.worktree));
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
