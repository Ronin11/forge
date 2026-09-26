//! Commands and their terminal output. The engine never prints; this does.

use crate::audit;
use crate::ctx::Forge;
use crate::profile::{self, LOOKBACK};
use crate::render;
use crate::store::{Task, TaskState};
use crate::{config, doctor, git, operation, unix_now, worker};
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Print to stdout without panicking when the reader has gone away, so
/// `forge log | head` is quiet.
macro_rules! out {
    ($($t:tt)*) => {{
        let mut o = std::io::stdout().lock();
        let _ = writeln!(o, $($t)*);
    }};
}

mod deploy;
mod gc;
mod initiatives;
mod jobs;
mod project_targets;
mod projects;
mod statistics;
mod stats;
mod task_records;
mod tasks;
mod web;
mod workflows;

use deploy::{DeployArgs, PluginCmd, ProvisionArgs};
use initiatives::InitiativeCmd;
use jobs::{EconomistCmd, ExperimentCmd, JobCmd, MessageCmd};
use projects::{IntakeCmd, ProjectCmd, RefCmd};
use stats::WebCmd;
use tasks::TaskCmd;
pub(crate) use web::web_token;
use workflows::WorkflowsCmd;

#[derive(Parser)]
#[command(name = "forge", about = "Forge")]
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
    /// The task in the customer's own words, for the day it was filed
    /// that way; shown on the portal's "Done" line instead of a derived
    /// one (see docs/PORTAL.md)
    #[arg(long)]
    title: Option<String>,
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
    /// Trust this task is filed at: operator, contact, or public (default:
    /// operator; see docs/GTM.md item 1). A plugin reaching a stranger's
    /// input, such as github-issues, should pass --trust public.
    #[arg(long)]
    trust: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run one task now
    Run(TaskArgs),
    /// Queue a task for `forge work`
    Add {
        #[command(flatten)]
        args: TaskArgs,
        /// Print `{"id", "queued"}` instead of the sentence
        #[arg(long)]
        json: bool,
    },
    /// The front door: sort a customer message into a request, a
    /// question, a need, or unclear, and act on it (see docs/INTAKE.md,
    /// "The front door is not the interview")
    Ask {
        /// The project the message is about
        project: String,
        /// The message, in the customer's own words
        message: String,
        /// The channel contact the message came from, addressed by a
        /// need's intake task or an unclear decision's question
        #[arg(long)]
        from: Option<String>,
    },
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
        /// Only tasks whose text, title, plan or last result summary
        /// contains this, or whose id is exactly this
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
        /// Only tasks whose recorded changes include this path, or
        /// anything under it when it names a directory (repeatable; a /
        /// boundary, so src/cli matches src/cli/tasks.rs and not
        /// src/client.rs). The changes are what the attempts' envelopes
        /// recorded: derived from git where the provider reports from
        /// git, else what the agent itself reported.
        #[arg(long = "touches")]
        touches: Vec<String>,
        /// With --touches: also tasks whose text mentions the path (a
        /// queued or running task has no changes yet); such rows are
        /// marked "by text"
        #[arg(long = "touches-text", requires = "touches")]
        touches_text: bool,
        /// Only tasks with an attempt that failed this verdict row
        /// (repeatable): a rule name such as changes-match-git or
        /// clean-tree, or a check name as the record spells it, such as
        /// test, clippy or task-check-1. An unknown name is refused,
        /// listing the known ones.
        #[arg(long = "failed-on")]
        failed_on: Vec<String>,
        /// Only tasks with an attempt whose reason contains this text
        #[arg(long)]
        reason: Option<String>,
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
        /// Who the answer came from: the operator, or (from a channel
        /// plugin) the contact the question was addressed to
        #[arg(long, default_value = "operator")]
        by: String,
        /// Restrict this answer to a project: refused unless the task
        /// belongs to this project and its question is addressed to
        /// `--by` (see `queue::answer`). Unset for the operator's own
        /// answers, which reach any task.
        #[arg(long)]
        project: Option<String>,
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
        /// Only decisions whose question, answer or citations contain this (case-insensitive)
        #[arg(long)]
        grep: Option<String>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Show one task and its attempts
    Show {
        id: i64,
        /// The task's full record as one JSON object (`TraceDoc.task`)
        #[arg(long)]
        json: bool,
    },
    /// Run the supervisor on a task blocked with a question, now
    Supervise { id: i64 },
    /// Set up FORGE_HOME on this machine: the data directory, the
    /// operator's config template, the workflow catalog as a committed
    /// git repository, web.token, and (when systemd is available) user
    /// units for the worker and web client, enabled with linger. Ends by
    /// running the same checks as `forge doctor`. Idempotent: a second
    /// run changes nothing and says so.
    Init {
        /// FORGE_HOME to set up (default: the usual resolution — FORGE_HOME,
        /// XDG_DATA_HOME/forge, or ~/.local/share/forge)
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Check this machine can run attempts and nothing is stuck
    Doctor {
        /// Machine-readable: a JSON array of {name, status, detail, hint}
        #[arg(long)]
        json: bool,
    },
    /// Print the crate version and, if built from a git checkout, its commit
    Version,
    /// Install a release tarball over the running binaries: verify it
    /// against its own SHA256SUMS, back the store up, keep the current
    /// binaries under <bin dir>/previous/, install the new ones, migrate
    /// the store through the new binary and report the schema version
    /// before and after, restart forge-web and forge-portal when their
    /// units exist and check the web client, then ask forge-worker to
    /// restart, last, after its drain. Refuses a tarball older than the
    /// running version unless --force; any failure restores the previous
    /// binaries. See docs/OPS.md, "Upgrading".
    Upgrade {
        /// A path to a release tarball, or a URL fetched with curl. A
        /// SHA256SUMS naming it must sit beside it (the same directory, or
        /// the same URL with SHA256SUMS in place of the tarball's name).
        source: Option<String>,
        /// Verify the tarball and report what would happen, without
        /// installing or restarting anything
        #[arg(long)]
        check_only: bool,
        /// Install even if the tarball's version is older than the
        /// running one (migrations do not run backwards; this does not
        /// undo them)
        #[arg(long)]
        force: bool,
    },
    /// Inside a sandbox: pipe loopback to the egress proxy's unix socket
    #[command(hide = true)]
    EgressRelay {
        #[arg(long, default_value = crate::egress::SANDBOX_SOCKET)]
        socket: std::path::PathBuf,
        #[arg(long, default_value = crate::egress::RELAY_ADDR)]
        listen: String,
        /// Created once the relay is listening
        #[arg(long)]
        ready: Option<std::path::PathBuf>,
    },
    /// List the workflows a task can run, with declared metadata and measured outcomes
    Workflows {
        #[command(subcommand)]
        cmd: Option<WorkflowsCmd>,
        /// Also list this project's own repository workflows
        /// (`.forge/workflows/`, at its latest landed commit), beside the
        /// operator's catalog (see docs/JOBS.md, "Where an automation lives")
        #[arg(long)]
        project: Option<String>,
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
    /// The module graph as data (docs/LATER.md, "The code visualiser"):
    /// files and symbols as nodes, import edges between them, files
    /// grouped under their directories as module nodes. Reads the
    /// repository at its working tree; the structure itself needs no
    /// task and no store. `--json` additionally overlays what the
    /// record knows about each file node (the operator's own store,
    /// when one is open): the tasks that changed it, review demotions
    /// naming it, and its share of repair cost.
    Graph {
        /// The repository's working tree
        repo: PathBuf,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Blocked tasks: questions for the operator and workflow requests
    Requests {
        /// Only requests for this repository
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Only requests whose question, tried or options contain this (case-insensitive)
        #[arg(long)]
        grep: Option<String>,
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
        /// Test-run means per role over the last N attempts (--last,
        /// default 50): forge-test calls and cache hits, raw test
        /// commands, full-suite runs, runs with no edit since the
        /// previous one, and test wall time
        #[arg(long)]
        tests: bool,
        /// With --tests, how many of the latest attempts to average
        #[arg(long, default_value_t = 50)]
        last: i64,
        /// Attempts, outcomes, cost and wall time per (role, provider,
        /// model), role being the attempt step
        #[arg(long)]
        by_role: bool,
        /// Attempts, outcomes and cost per (workflow, step, prompt hash):
        /// two versions of a directive's prompt are two rows
        #[arg(long)]
        by_step: bool,
        /// Landing rate and true cost per factor level (provider per
        /// role, workflow, size class), with a main-effects fit of log
        /// true cost across all of them (see docs/ECONOMIST.md)
        #[arg(long)]
        factors: bool,
        /// Every task that blocked with a question: per kind, the count,
        /// who settled it (supervisor, operator, withdrawn), how many
        /// were "do it as stated", the median hours waited, and the
        /// operator's attention cost at [measure] operator_usd_per_hour
        #[arg(long)]
        questions: bool,
        /// With --factors or --questions, only tasks that finished (or
        /// blocked) in the last N days
        #[arg(long)]
        days: Option<i64>,
        /// Only this project's tasks (also adds the per-project section
        /// when neither this nor --initiative is given)
        #[arg(long)]
        project: Option<String>,
        /// Only this initiative's tasks
        #[arg(long)]
        initiative: Option<i64>,
        /// Set cost_usd from recorded tokens on every attempt whose
        /// provider reported no cost (cost_usd 0 or NULL) but whose
        /// provider has prices in the operator config; records the run
        /// as a decision row (see docs/ECONOMIST.md, "Repricing a
        /// free-reporting provider")
        #[arg(long)]
        reprice: bool,
        /// With --reprice, only this provider's attempts
        #[arg(long)]
        provider: Option<String>,
        /// With --reprice, redo attempts this command already repriced
        #[arg(long)]
        force: bool,
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
        /// Also remove a retained worktree with unpublished commits once
        /// its task finished at least this many days ago, pushing its
        /// branch to the registered repository or the remote first so
        /// nothing is lost.
        #[arg(long)]
        older_than: Option<i64>,
    },
    /// Plugins: integrations discovered under FORGE_HOME/plugins and plugin_dirs
    Plugin {
        #[command(subcommand)]
        cmd: PluginCmd,
    },
    /// External references on a task: the pull request it landed as, the issue it came from
    Ref {
        #[command(subcommand)]
        cmd: RefCmd,
    },
    /// The message record: what a contact said on a channel and what was
    /// said back, so a rule can ask "has this contact replied since"
    /// (see docs/PLUGINS.md)
    Message {
        #[command(subcommand)]
        cmd: MessageCmd,
    },
    /// Projects: the unit of ownership above a task (see docs/PROJECTS.md)
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Jobs: one run of a `kind = "run"` workflow (see docs/JOBS.md)
    Job {
        #[command(subcommand)]
        cmd: JobCmd,
    },
    /// Initiatives: one outcome, pursued as a set of tasks, tracked as
    /// one thing (see docs/PROJECTS.md)
    Initiative {
        #[command(subcommand)]
        cmd: InitiativeCmd,
    },
    /// Change an existing task
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    /// Run a deploy target now, or (with `log`) show what was deployed
    /// when (see docs/DEPLOY.md)
    Deploy(DeployArgs),
    /// Provision a Hetzner Cloud box for a project's deploy target,
    /// recording its ipv4 as the target's host arg (see docs/DEPLOY.md,
    /// "Provisioning")
    Provision(ProvisionArgs),
    /// Intake: turning a confirmed interview brief into a project (see docs/INTAKE.md)
    Intake {
        #[command(subcommand)]
        cmd: IntakeCmd,
    },
    /// The economist: randomized assignment and its weekly rebalance (see
    /// docs/ECONOMIST.md, "What is built")
    Economist {
        #[command(subcommand)]
        cmd: EconomistCmd,
    },
    /// `experiment.toml` directly, beside the workflow catalog (see
    /// docs/ECONOMIST.md)
    Experiment {
        #[command(subcommand)]
        cmd: ExperimentCmd,
    },
    /// The operator's own way to reach a running (or not-yet-started)
    /// `forge-web`: the tokened link it prints at start, without having
    /// to start a second one just to see it (see docs/CLIENT.md,
    /// "Reaching forge-web")
    Web {
        #[command(subcommand)]
        cmd: WebCmd,
    },
}

pub async fn main() -> Result<()> {
    let cmd = Cli::parse().cmd;
    match cmd {
        Cmd::Run(..)
        | Cmd::Add { .. }
        | Cmd::Work { .. }
        | Cmd::Log { .. }
        | Cmd::Retry { .. }
        | Cmd::Answer { .. }
        | Cmd::Withdraw { .. }
        | Cmd::Decisions { .. }
        | Cmd::Show { .. }
        | Cmd::Supervise { .. }
        | Cmd::Trace { .. }
        | Cmd::Integrate { .. }
        | Cmd::Land { .. }
        | Cmd::Journal { .. }
        | Cmd::Task { .. } => tasks::dispatch(cmd).await,
        Cmd::Ref { .. } | Cmd::Project { .. } | Cmd::Initiative { .. } | Cmd::Intake { .. } => {
            projects::dispatch(cmd).await
        }
        Cmd::Ask { .. }
        | Cmd::Message { .. }
        | Cmd::Job { .. }
        | Cmd::Economist { .. }
        | Cmd::Experiment { .. } => jobs::dispatch(cmd).await,
        Cmd::Workflows { .. } | Cmd::Providers { .. } => workflows::dispatch(cmd).await,
        Cmd::Plugin { .. } | Cmd::Deploy(..) | Cmd::Provision(..) => deploy::dispatch(cmd).await,
        Cmd::Gc { .. }
        | Cmd::Init { .. }
        | Cmd::Doctor { .. }
        | Cmd::Version
        | Cmd::Upgrade { .. }
        | Cmd::EgressRelay { .. }
        | Cmd::Graph { .. }
        | Cmd::Requests { .. }
        | Cmd::Stats { .. }
        | Cmd::Events { .. }
        | Cmd::Snapshot
        | Cmd::Web { .. } => stats::dispatch(cmd).await,
    }
}

impl From<&TaskArgs> for crate::queue::TaskRequest {
    fn from(a: &TaskArgs) -> Self {
        crate::queue::TaskRequest {
            repo: a.repo.clone(),
            task: a.task.clone(),
            title: a.title.clone(),
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
            trust: a.trust.clone(),
        }
    }
}

fn tasks_json(f: &Forge, q: &crate::store::TaskFilter) -> Result<Vec<crate::view::TaskRow>> {
    crate::view::task_rows(f, q)
}

fn requests_json(
    f: &Forge,
    repo: Option<&str>,
    grep: Option<&str>,
) -> Result<Vec<crate::view::RequestRow>> {
    Ok(f.store
        .blocked(repo, grep)?
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
            let delivered_at = t
                .question_to
                .as_deref()
                .and_then(|to| f.store.delivered_at(t.id, to).ok().flatten());
            crate::view::RequestRow {
                delivered_at,
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
            serde_json::json!({"running": w.running, "pid": w.pid, "exe": w.exe, "stale_binary": w.stale.unwrap_or(false)})
        }
    }
}

#[cfg(test)]
mod tests;
