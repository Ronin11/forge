//! One SQLite file, two tables. A task is what the operator asked for; an
//! attempt is one run of the agent against it. The numbers live on attempts
//! and every one of them was computed by Forge, git, or the CLI's
//! accounting. Migrations are forward-only and numbered by `user_version`.

use anyhow::{Context, Result, bail};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TaskState {
    #[default]
    Queued,
    Running,
    Succeeded,
    Failed,
    Unverified,
    /// The agent asked a question or for a different workflow; not a failure.
    Blocked,
    /// The operator decided this should not be done: a stale description,
    /// superseded, or the product decision went the other way. Terminal,
    /// like `Failed`, but never a defect in the work.
    Withdrawn,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Queued => "queued",
            TaskState::Running => "running",
            TaskState::Succeeded => "succeeded",
            TaskState::Failed => "failed",
            TaskState::Unverified => "unverified",
            TaskState::Blocked => "blocked",
            TaskState::Withdrawn => "withdrawn",
        }
    }
}

impl TryFrom<&str> for TaskState {
    type Error = std::io::Error;
    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        Ok(match s {
            "queued" => TaskState::Queued,
            "running" => TaskState::Running,
            "succeeded" => TaskState::Succeeded,
            "failed" => TaskState::Failed,
            "unverified" => TaskState::Unverified,
            "blocked" => TaskState::Blocked,
            "withdrawn" => TaskState::Withdrawn,
            other => {
                return Err(std::io::Error::other(format!(
                    "unknown task state {other:?}"
                )));
            }
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AttemptState {
    #[default]
    Running,
    Succeeded,
    ChecksFailed,
    AgentFailed,
    Unverified,
    /// The agent asked the operator a question; retrying cannot answer it.
    NeedsInput,
}

impl AttemptState {
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptState::Running => "running",
            AttemptState::Succeeded => "succeeded",
            AttemptState::ChecksFailed => "checks_failed",
            AttemptState::AgentFailed => "agent_failed",
            AttemptState::Unverified => "unverified",
            AttemptState::NeedsInput => "needs_input",
        }
    }
}

impl TryFrom<&str> for AttemptState {
    type Error = std::io::Error;
    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        Ok(match s {
            "running" => AttemptState::Running,
            "succeeded" => AttemptState::Succeeded,
            "checks_failed" => AttemptState::ChecksFailed,
            "agent_failed" => AttemptState::AgentFailed,
            "unverified" => AttemptState::Unverified,
            "needs_input" => AttemptState::NeedsInput,
            other => {
                return Err(std::io::Error::other(format!(
                    "unknown attempt state {other:?}"
                )));
            }
        })
    }
}

#[derive(Default, Debug, Clone)]
pub struct Task {
    pub id: i64,
    pub repo: String,
    pub task: String,
    pub base_branch: String,
    pub base_sha: String,
    pub branch: String,
    pub worktree: String,
    pub model: String,
    /// The provider name every agent step of this task runs under (see
    /// `agent::Provider`); the supervisor keeps its own model setting and
    /// is unaffected by this.
    pub provider: String,
    pub max_turns: i64,
    pub max_attempts: i64,
    pub timeout_secs: i64,
    /// Operator-declared acceptance commands, run as L2 after the repo's checks.
    pub checks: Vec<String>,
    pub state: TaskState,
    pub reason: String,
    /// Who a blocked question is addressed to (a channel contact's name,
    /// e.g. from the Signal plugin's `CONTACTS`); `None` means the
    /// operator. Set from the envelope's `needs_input.to` when the task
    /// blocks; meaningless outside `TaskState::Blocked`.
    pub question_to: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub pushed: bool,
    pub worker_pid: Option<i64>,
    /// Per-task cap override; `None` means the operator config's default.
    pub budget_usd: Option<f64>,
    pub worktree_removed_at: Option<i64>,
    /// The operator said this task may change protected paths.
    pub allow_protected: bool,
    pub workflow: String,
    /// Content hash of the workflow file the task ran under.
    pub workflow_hash: String,
    /// The workflow file's exact text at resolution, so the run is
    /// self-describing even after the file changes.
    pub workflow_text: String,
    /// workflows::Resolved as JSON: every action version the task runs,
    /// recorded at start; empty until then.
    pub actions_json: String,
    /// The tests step's summary: what the coder is told about the tests.
    pub interface: String,
    /// Show the L2 acceptance commands to the coder (default hidden).
    pub show_checks: bool,
    /// Land on the base branch once verified (the default); false leaves
    /// the verified branch pushed for a human to merge.
    pub land: bool,
    /// Tasks this one waits for: claimable only once every one of them has
    /// landed; blocked if any of them ends otherwise.
    pub after: Vec<i64>,
    /// The `forge-verify` commit that matches the base at clone time: the
    /// standing suite the task is judged by. Landing uses the current tip,
    /// since only the merged tree has everything the base gained since.
    pub verify_base: String,
    /// The task this one re-queues, when it was made by `forge retry`.
    pub retry_of: Option<i64>,
    /// Show the agents the journal of earlier attempts (the default);
    /// false for the control arm of a measurement.
    pub journal: bool,
    /// How `journal` got its value: "explicit" when the request said
    /// `--journal` or `--no-journal` itself, else "control" or "treatment"
    /// from the operator's `[measure] journal_control` fraction, drawn
    /// deterministically from the task id.
    pub journal_arm: String,
    /// What the last `context` operation printed: where things are.
    pub context: String,
    /// Show the agents that context (the default); false for the control arm.
    pub context_enabled: bool,
    /// After an attempt fails its checks, hand the next one --resume with
    /// the same CLI session instead of a fresh one. Preserved by `forge retry`.
    pub resume_on_failure: bool,
    /// What the last `plan` directive returned: the plan every later
    /// directive on this task is shown.
    pub plan: String,
    /// The base commit the task's branch became, once landed; empty until
    /// then. The scheduler's notion of "landed" is this column, not the
    /// wording of `reason`.
    pub landed_sha: String,
    /// The project this task belongs to; `None` for tasks predating
    /// projects that no migration could place, or whose repository lists
    /// more than one project.
    pub project: Option<String>,
    /// The initiative this task belongs to, if any.
    pub initiative: Option<i64>,
    /// Which provider each role drew from the operator's `[measure]
    /// explore` fractions, keyed by role name; empty when the task named
    /// an explicit `--provider` (which routes every role itself) or no
    /// role was configured to explore. Drawn once at creation, the same
    /// deterministic way as `journal_arm` (see `queue::assign_explore`),
    /// and consulted by `ctx::resolve_provider` at every step.
    pub explore: BTreeMap<String, String>,
}

#[derive(Default, Debug, Clone)]
pub struct Attempt {
    pub id: i64,
    pub task_id: i64,
    pub attempt_no: i64,
    pub step: String,
    /// Index of the step in the resolved workflow, for resumption.
    pub step_seq: i64,
    /// HEAD when the attempt started: "what you changed" means since here.
    pub start_sha: String,
    pub end_sha: String,
    /// The runner and provider this attempt ran under (see
    /// `agent::Runner`/`agent::Provider`); the model is on `inputs_json`.
    pub runner: String,
    pub provider: String,
    /// audit::Inputs as JSON: everything the step was given.
    pub inputs_json: String,
    /// audit::Outputs as JSON: everything the step produced beyond the verdict.
    pub outputs_json: String,
    pub state: AttemptState,
    pub reason: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub agent_exit: Option<i32>,
    pub timed_out: bool,
    pub num_turns: i64,
    pub tool_calls: i64,
    pub cost_usd: Option<f64>,
    pub agent_ms: i64,
    pub commits: i64,
    pub files_changed: i64,
    pub dirty: bool,
    pub verdict_json: String,
    pub result_text: String,
    pub log_path: String,
    /// The structured result as the CLI produced it, raw JSON; empty if none.
    pub envelope_json: String,
    pub rl_five_hour: Option<f64>,
    pub rl_seven_day: Option<f64>,
    pub rl_five_hour_resets: Option<i64>,
    pub rl_seven_day_resets: Option<i64>,
    /// The CLI session the attempt ran in; empty when the stream never said.
    pub session_id: String,
    /// Tool calls before the first edit; `None` when it never edited.
    pub first_edit: Option<i64>,
    /// Token counts from the result frame's usage object.
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    /// `agent::Outcome::early_signals` as JSON: which of `Watch`'s signs
    /// tripped, whether or not they ended the run.
    pub early_signals: String,
    /// `agent::Outcome::early_near` as JSON: which signs were within 20%
    /// of tripping and did not, so the thresholds can be tuned from here.
    pub early_near: String,
}

/// Everything `Store::finish_attempt` writes back for an attempt that has run to completion.
pub struct FinishAttempt {
    /// The attempt row to update.
    pub id: i64,
    pub state: AttemptState,
    pub reason: String,
    pub finished_at: Option<i64>,
    pub agent_exit: Option<i32>,
    pub timed_out: bool,
    pub num_turns: i64,
    pub tool_calls: i64,
    pub cost_usd: Option<f64>,
    pub agent_ms: i64,
    pub commits: i64,
    pub files_changed: i64,
    pub dirty: bool,
    pub verdict_json: String,
    pub result_text: String,
    /// The structured result as the CLI produced it, raw JSON; empty if none.
    pub envelope_json: String,
    pub rl_five_hour: Option<f64>,
    pub rl_seven_day: Option<f64>,
    /// Unix seconds at which each window resets, as the CLI reported.
    pub rl_five_hour_resets: Option<i64>,
    pub rl_seven_day_resets: Option<i64>,
    /// HEAD when the attempt finished.
    pub end_sha: String,
    /// audit::Outputs as JSON: everything the step produced beyond the verdict.
    pub outputs_json: String,
    /// The CLI session the attempt ran in; empty when the stream never said.
    pub session_id: String,
    /// Tool calls before the first edit; `None` when it never edited.
    pub first_edit: Option<i64>,
    /// Token counts from the result frame's usage object.
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub early_signals: String,
    pub early_near: String,
}

pub struct RateLimitSample {
    pub seen_at: i64,
    pub five_hour: Option<f64>,
    pub seven_day: Option<f64>,
    /// Unix seconds at which each window resets, as the CLI reported.
    pub five_hour_resets: Option<i64>,
    pub seven_day_resets: Option<i64>,
}

pub struct WorkflowStat {
    pub workflow: String,
    pub hash: String,
    pub tasks: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub blocked: i64,
    pub unverified: i64,
    pub cost: f64,
    pub attempts: i64,
    pub landed: i64,
    /// Landed tasks whose `landed_sha` became a later task's `base_sha`,
    /// where that later task's first `code` attempt carries a failing L1
    /// verdict row: the base was already broken when the next task started.
    pub broke_base: i64,
    /// Landed tasks named by another task's `repairs` reference
    /// (`forge://task/<id>`).
    pub repaired: i64,
    /// Delayed cost, summed across this workflow's landed tasks (the
    /// `task_repair_cost` cache): for each later landing on the same
    /// repository within `THIRTY_DAYS_SECS`, the fraction of its cost
    /// equal to the lines it removed or rewrote that this landed task
    /// added, divided by all the lines it removed or rewrote. A later
    /// landing that rewrote none of a task's lines charges it nothing; one
    /// that rewrote all of it and nothing else charges it in full. A later
    /// landing following on from two landed tasks in this workflow has its
    /// cost split between them by how many of each task's lines it
    /// rewrote.
    pub repair_cost: f64,
    /// Lines this workflow's landed tasks added, summed from the
    /// `task_churn` cache.
    pub added_lines: i64,
    /// Of `added_lines`, how many a later landing on the same repository
    /// removed or rewrote within `THIRTY_DAYS_SECS` (`task_churn`), summed.
    pub churned_lines: i64,
}

pub struct StepStat {
    pub workflow: String,
    pub step: String,
    pub attempts: i64,
    pub succeeded: i64,
    pub agent_failed: i64,
    pub checks_failed: i64,
    pub needs_input: i64,
    pub mean_turns: f64,
    pub cost: f64,
    pub mean_ms: f64,
    /// Mean tool calls before the first edit, over attempts that edited.
    pub mean_first_edit: Option<f64>,
    /// Mean input tokens, over attempts that reported usage.
    pub mean_input_tokens: Option<f64>,
}

/// One side of the journal control arm's retrospective split: code attempts
/// after the first (`attempt_no > 1`), grouped by whether `inputs_json`'s
/// `journal` field was present and non-empty. See docs/LATER.md, "The
/// journal measurement was ill-posed three times".
pub struct JournalStat {
    pub has_journal: bool,
    pub attempts: i64,
    pub succeeded: i64,
    pub mean_turns: f64,
    /// Mean tool calls before the first edit, over attempts that edited.
    pub mean_first_edit: Option<f64>,
    /// Mean cost in USD per attempt.
    pub mean_cost_usd: f64,
}

/// One row of the runner breakdown: attempts, outcomes, cost and wall
/// time for one (role, provider, model) combination, role being the
/// attempt's step (see `forge stats --by-role`).
pub struct RoleStat {
    pub role: String,
    pub provider: String,
    pub model: String,
    pub attempts: i64,
    pub succeeded: i64,
    pub mean_turns: f64,
    pub mean_cost_usd: f64,
    pub mean_ms: f64,
    /// Landed tasks with an attempt in this group, and how many broke a
    /// later task's base (see `WorkflowStat::broke_base`); `None` outside
    /// the `code` role, where landing is not meaningful.
    pub landed: Option<i64>,
    pub broke_base: Option<i64>,
    /// See `WorkflowStat::repair_cost`, summed over this group's own
    /// landed tasks; `None` outside the `code` role.
    pub repair_cost: Option<f64>,
    /// See `WorkflowStat::added_lines`/`churned_lines`, summed over this
    /// group's own landed tasks; `None` outside the `code` role.
    pub added_lines: Option<i64>,
    pub churned_lines: Option<i64>,
}

/// One operation, kernel or user, as it ran.
#[derive(Default, Debug, Clone)]
pub struct Op {
    pub id: i64,
    pub task_id: i64,
    pub seq: i64,
    pub name: String,
    pub kernel: bool,
    pub started_at: i64,
    pub ms: i64,
    pub ok: bool,
    pub exit: Option<i32>,
    pub detail: String,
    pub attempt_id: Option<i64>,
    /// What the operation produced, when it produces a value: its stdout.
    pub output: String,
}

/// One task in a lineage: parent is what it retries.
#[derive(Debug, Clone)]
pub struct LineageRow {
    pub id: i64,
    pub parent: Option<i64>,
    pub state: String,
    pub reason: String,
    pub workflow: String,
    pub cost: f64,
}

/// An operator's answer to a blocked task's question.
pub struct Decision {
    pub id: i64,
    pub task_id: i64,
    pub repo: String,
    pub question: String,
    pub answer: String,
    pub created_at: i64,
    /// "operator", "supervisor", or a channel contact's name.
    pub answered_by: String,
    /// What the answer cited, comma-separated: paths, "task N", "decision N".
    pub citations: String,
    /// The task the answer re-queued, when known: its state is the
    /// answer's outcome.
    pub retry_id: Option<i64>,
    /// Who the question was addressed to, copied from the task's
    /// `question_to` at answer time; `None` means the operator.
    pub answered_for: Option<String>,
}

/// An external reference a plugin or the operator recorded on a task: the
/// pull request it landed as, the issue it came from.
pub struct TaskRef {
    pub id: i64,
    pub task_id: i64,
    pub kind: String,
    pub url: String,
    pub label: String,
    /// Who recorded it: "operator" by default, or a plugin's own name.
    pub by: String,
    pub created_at: i64,
}

/// What `forge log` filters on.
#[derive(Default, Debug, Clone)]
pub struct TaskFilter {
    pub limit: u32,
    pub state: Option<TaskState>,
    pub repo: Option<String>,
    /// Only ids strictly below this one: the next page when scrolling back.
    pub before: Option<i64>,
    /// A substring of the task text, or an exact id.
    pub grep: Option<String>,
    pub workflow: Option<String>,
    /// Only this project's tasks.
    pub project: Option<String>,
    /// Only this initiative's tasks.
    pub initiative: Option<i64>,
}

/// What `forge decisions` filters on, and what the supervisor's prompt
/// scopes its own reading of the record to (see docs/PROJECTS.md, "The
/// record, scoped"): a decision has no `project`/`initiative` column of
/// its own, so these narrow through the task it was recorded on.
#[derive(Default, Debug, Clone)]
pub struct DecisionFilter {
    pub repo: Option<String>,
    pub project: Option<String>,
    pub initiative: Option<i64>,
}

/// What `forge stats` filters on: the same project/initiative scope as
/// `TaskFilter` and `DecisionFilter`, without the paging/grep fields that
/// only `forge log`'s raw listing needs.
#[derive(Default, Debug, Clone)]
pub struct StatsFilter {
    pub project: Option<String>,
    pub initiative: Option<i64>,
}

pub struct TaskSummary {
    pub id: i64,
    pub state: String,
    pub workflow: String,
    pub created: String,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub repo: String,
    pub task: String,
    pub attempts: i64,
    pub cost: f64,
    pub project: Option<String>,
    pub initiative: Option<i64>,
}

/// The unit of ownership above a task: what is being built, and for whom.
/// Defaults are nullable: `None` means this project sets nothing for that
/// column, and resolution falls through to the next layer (see
/// docs/PROJECTS.md, "Configuration layering").
#[derive(Default, Debug, Clone)]
pub struct Project {
    pub name: String,
    pub purpose: String,
    pub created_at: i64,
    pub workflow: Option<String>,
    pub per_task_usd: Option<f64>,
    pub per_initiative_usd: Option<f64>,
    pub supervisor_model: Option<String>,
    pub supervisor_per_lineage: Option<i64>,
    /// Extra protected paths, on top of the repository's own `forge.toml`.
    pub protected: Option<Vec<String>>,
    /// Which provider a role runs under, for roles this project overrides
    /// (see `config::ROLES`); a role missing here falls to the operator's
    /// `[roles]` table. Set with `forge project set --role <role>=<provider>`.
    pub role_providers: BTreeMap<String, String>,
}

/// What `forge project set` changes; a field left `None` keeps the
/// project's current value for that column. There is no way to clear a
/// column back to unset once set, which nothing here needs yet.
#[derive(Default, Debug, Clone)]
pub struct ProjectDefaults {
    pub workflow: Option<String>,
    pub per_task_usd: Option<f64>,
    pub per_initiative_usd: Option<f64>,
    pub supervisor_model: Option<String>,
    pub supervisor_per_lineage: Option<i64>,
    pub protected: Option<Vec<String>>,
    /// Role/provider pairs to merge into the project's existing
    /// `role_providers`; a role already set keeps its old value unless
    /// named again here. Empty changes nothing.
    pub role_providers: BTreeMap<String, String>,
}

/// One repository a project works in, and the paths it owns there;
/// `scope` is `None` for the whole repository.
#[derive(Debug, Clone)]
pub struct ProjectRepo {
    pub repo: String,
    pub scope: Option<String>,
}

/// One backlog item: a thing worth doing that is not yet queued.
#[derive(Debug, Clone)]
pub struct BacklogItem {
    pub id: i64,
    pub project: String,
    pub text: String,
    pub created_at: i64,
    pub done_at: Option<i64>,
}

/// A deploy target: where a project's landed code runs, how it gets
/// there, and what proves it is up (see docs/DEPLOY.md, "A target").
/// `scope` is the raw JSON array of paths within `repo` the target
/// deploys, `None` for the whole repository, mirroring `ProjectRepo`.
#[derive(Debug, Clone)]
pub struct DeployTarget {
    pub project: String,
    pub name: String,
    pub repo: String,
    pub scope: Option<String>,
    /// The action file this target runs, e.g. "deploy-command".
    pub method: String,
    pub args: BTreeMap<String, String>,
    pub check_cmd: String,
    pub on_landing: bool,
    /// A url the deploy-smoke operation opens in headless Chromium after
    /// the check passes, `None` to skip the smoke step entirely (see
    /// docs/DEPLOY.md, "A deterministic smoke step").
    pub smoke_url: Option<String>,
}

/// One deploy: a target, the commit deployed, when it started and
/// finished, the check's verdict and output, and what it rolled back to
/// if the check failed (see docs/DEPLOY.md, "When a deploy runs").
#[derive(Debug, Clone)]
pub struct Deploy {
    pub id: i64,
    pub project: String,
    pub target: String,
    pub sha: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub check_ok: Option<bool>,
    pub check_output: String,
    pub rolled_back_to: Option<String>,
    pub reason: String,
    /// The task this deploy ran on behalf of, when it was an on-landing
    /// target rather than an operator-invoked `forge deploy`. Recorded now;
    /// surfaced to a view once a later step needs it.
    #[allow(dead_code)]
    pub task_id: Option<i64>,
    /// Whether the deploy-smoke operation passed, `None` when the target
    /// declares no smoke url or the check never passed for smoke to run.
    pub smoke_ok: Option<bool>,
    /// The smoke operation's own record: console errors, failed requests,
    /// title and screenshot path, as the JSON it wrote (see
    /// src/builtins/operations/deploy-smoke.toml).
    pub smoke_json: Option<String>,
}

/// One run of the assess directive against a landed task (see
/// src/assess.rs): a maintainability score 0-10 and a list of findings, as
/// the JSON `[{"path":...,"finding":...,"severity":"notable"|"concern"}]`
/// the directive returned, with what ran it and what it cost. Surfaced by
/// `forge show`, `forge trace --json` and `forge initiative report`;
/// never by `forge stats`.
#[derive(Debug, Clone)]
pub struct Assessment {
    /// Recorded now; no view needs it (each reads a task's single most
    /// recent assessment by `task_id`, not this row's own id).
    #[allow(dead_code)]
    pub id: i64,
    pub task_id: i64,
    pub score: i64,
    pub findings_json: String,
    pub model: String,
    pub provider: String,
    pub cost_usd: Option<f64>,
    pub created_at: i64,
}

/// The unit of operation above a task: one outcome, pursued as a set of
/// tasks, tracked as one thing (see docs/PROJECTS.md, "Initiative").
/// `budget_usd` and `stop_after_same_rule` are nullable-in-spirit only for
/// the budget: `None` falls to the project's `per_initiative_usd`, while
/// the stop rule always has a value (the schema default, 3, when the
/// operator names none).
#[derive(Default, Debug, Clone)]
pub struct Initiative {
    pub id: i64,
    pub project: String,
    pub outcome: String,
    pub budget_usd: Option<f64>,
    pub stop_after_same_rule: i64,
    pub created_at: i64,
    /// When every task settled and the initiative's own record closed;
    /// `None` while it is still open or held.
    pub settled_at: Option<i64>,
}

/// A change to an existing initiative: only the fields given replace the
/// stored value, the rest are left alone (see `Store::set_initiative`).
#[derive(Default, Debug, Clone)]
pub struct InitiativeUpdate {
    pub outcome: Option<String>,
    pub budget_usd: Option<f64>,
    pub stop_after_same_rule: Option<i64>,
}

/// Task counts by state and total cost for one project.
#[derive(Default, Debug, Clone)]
pub struct ProjectTaskStats {
    pub queued: i64,
    pub running: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub unverified: i64,
    pub blocked: i64,
    pub withdrawn: i64,
    pub cost: f64,
}

/// Tasks, landed count, cost and defect escape for one project, as
/// `forge stats`'s per-project section shows it.
#[derive(Default, Debug, Clone)]
pub struct ProjectStat {
    pub project: String,
    pub tasks: i64,
    pub landed: i64,
    pub cost: f64,
    /// Landed tasks whose `landed_sha` became a later task's `base_sha`,
    /// where that later task's first `code` attempt carries a failing L1
    /// verdict row on an unmodified base (see `WorkflowStat::broke_base`).
    pub broke_base: i64,
}

pub struct Store {
    conn: Mutex<Connection>,
}

/// Forward-only. Index = version - 1. Never edit a shipped entry; append.
pub const MIGRATIONS: &[&str] = &[
    "
CREATE TABLE tasks (
  id INTEGER PRIMARY KEY,
  repo TEXT NOT NULL,
  task TEXT NOT NULL,
  base_branch TEXT NOT NULL,
  base_sha TEXT NOT NULL DEFAULT '',
  branch TEXT NOT NULL DEFAULT '',
  worktree TEXT NOT NULL DEFAULT '',
  model TEXT NOT NULL,
  max_turns INTEGER NOT NULL,
  max_attempts INTEGER NOT NULL,
  timeout_secs INTEGER NOT NULL,
  checks_json TEXT NOT NULL DEFAULT '[]',
  state TEXT NOT NULL,
  reason TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  started_at INTEGER,
  finished_at INTEGER,
  pushed INTEGER NOT NULL DEFAULT 0,
  worker_pid INTEGER,
  budget_usd REAL,
  worktree_removed_at INTEGER
);
CREATE TABLE attempts (
  id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(id),
  attempt_no INTEGER NOT NULL,
  state TEXT NOT NULL,
  reason TEXT NOT NULL DEFAULT '',
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  agent_exit INTEGER,
  timed_out INTEGER NOT NULL DEFAULT 0,
  num_turns INTEGER NOT NULL DEFAULT 0,
  tool_calls INTEGER NOT NULL DEFAULT 0,
  cost_usd REAL,
  agent_ms INTEGER NOT NULL DEFAULT 0,
  commits INTEGER NOT NULL DEFAULT 0,
  files_changed INTEGER NOT NULL DEFAULT 0,
  dirty INTEGER NOT NULL DEFAULT 0,
  verdict_json TEXT NOT NULL DEFAULT '[]',
  result_text TEXT NOT NULL DEFAULT '',
  log_path TEXT NOT NULL DEFAULT ''
);
CREATE INDEX attempts_task ON attempts(task_id, attempt_no);
CREATE INDEX tasks_state ON tasks(state, id);
",
    "
ALTER TABLE attempts ADD COLUMN envelope_json TEXT NOT NULL DEFAULT '';
ALTER TABLE attempts ADD COLUMN rl_five_hour REAL;
ALTER TABLE attempts ADD COLUMN rl_seven_day REAL;
ALTER TABLE attempts ADD COLUMN rl_five_hour_resets INTEGER;
ALTER TABLE attempts ADD COLUMN rl_seven_day_resets INTEGER;
",
    "
ALTER TABLE tasks ADD COLUMN allow_protected INTEGER NOT NULL DEFAULT 0;
",
    "
ALTER TABLE tasks ADD COLUMN workflow TEXT NOT NULL DEFAULT 'direct';
ALTER TABLE tasks ADD COLUMN interface TEXT NOT NULL DEFAULT '';
ALTER TABLE tasks ADD COLUMN show_checks INTEGER NOT NULL DEFAULT 0;
ALTER TABLE attempts ADD COLUMN step TEXT NOT NULL DEFAULT 'code';
",
    "
ALTER TABLE attempts ADD COLUMN start_sha TEXT NOT NULL DEFAULT '';
ALTER TABLE tasks ADD COLUMN workflow_hash TEXT NOT NULL DEFAULT '';
",
    "
ALTER TABLE tasks ADD COLUMN workflow_text TEXT NOT NULL DEFAULT '';
ALTER TABLE attempts ADD COLUMN end_sha TEXT NOT NULL DEFAULT '';
ALTER TABLE attempts ADD COLUMN inputs_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE attempts ADD COLUMN outputs_json TEXT NOT NULL DEFAULT '{}';
",
    "
ALTER TABLE tasks ADD COLUMN actions_json TEXT NOT NULL DEFAULT '';
ALTER TABLE attempts ADD COLUMN step_seq INTEGER NOT NULL DEFAULT 0;
CREATE TABLE ops (
  id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(id),
  seq INTEGER NOT NULL,
  name TEXT NOT NULL,
  kernel INTEGER NOT NULL,
  started_at INTEGER NOT NULL,
  ms INTEGER NOT NULL DEFAULT 0,
  ok INTEGER NOT NULL DEFAULT 0,
  exit INTEGER,
  detail TEXT NOT NULL DEFAULT '',
  attempt_id INTEGER
);
CREATE INDEX ops_task ON ops(task_id, id);
",
    "
ALTER TABLE ops ADD COLUMN output TEXT NOT NULL DEFAULT '';
",
    "
ALTER TABLE tasks ADD COLUMN land INTEGER NOT NULL DEFAULT 1;
",
    "
ALTER TABLE attempts ADD COLUMN session_id TEXT NOT NULL DEFAULT '';
",
    "
ALTER TABLE tasks ADD COLUMN after_json TEXT NOT NULL DEFAULT '[]';
",
    "
ALTER TABLE tasks ADD COLUMN verify_base TEXT NOT NULL DEFAULT '';
",
    "
ALTER TABLE tasks ADD COLUMN retry_of INTEGER;
",
    "
ALTER TABLE attempts ADD COLUMN first_edit INTEGER;
",
    "
ALTER TABLE tasks ADD COLUMN journal INTEGER NOT NULL DEFAULT 1;
",
    "
ALTER TABLE attempts ADD COLUMN input_tokens INTEGER;
ALTER TABLE attempts ADD COLUMN output_tokens INTEGER;
ALTER TABLE attempts ADD COLUMN cache_read_input_tokens INTEGER;
ALTER TABLE attempts ADD COLUMN cache_creation_input_tokens INTEGER;
",
    "
ALTER TABLE tasks ADD COLUMN context TEXT NOT NULL DEFAULT '';
ALTER TABLE tasks ADD COLUMN context_enabled INTEGER NOT NULL DEFAULT 1;
",
    "
ALTER TABLE tasks ADD COLUMN resume_on_failure INTEGER NOT NULL DEFAULT 0;
",
    "
CREATE TABLE decisions (
  id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(id),
  repo TEXT NOT NULL,
  question TEXT NOT NULL,
  answer TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
",
    "
ALTER TABLE tasks ADD COLUMN plan TEXT NOT NULL DEFAULT '';
",
    "
ALTER TABLE decisions ADD COLUMN answered_by TEXT NOT NULL DEFAULT 'operator';
ALTER TABLE decisions ADD COLUMN citations TEXT NOT NULL DEFAULT '';
ALTER TABLE decisions ADD COLUMN retry_id INTEGER;
",
    "
ALTER TABLE tasks ADD COLUMN landed_sha TEXT NOT NULL DEFAULT '';
UPDATE tasks SET landed_sha = substr(reason, instr(reason, '@ ') + 2, 8) WHERE reason LIKE 'landed %' AND instr(reason, '@ ') > 0;
",
    "
CREATE TABLE plugins (
  name TEXT PRIMARY KEY,
  enabled INTEGER NOT NULL DEFAULT 0,
  enabled_at INTEGER
);
",
    "
CREATE TABLE task_refs (
  id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(id),
  kind TEXT NOT NULL,
  url TEXT NOT NULL,
  label TEXT NOT NULL DEFAULT '',
  by TEXT NOT NULL DEFAULT 'operator',
  created_at INTEGER NOT NULL
);
CREATE INDEX task_refs_task ON task_refs(task_id, id);
",
    "
ALTER TABLE attempts ADD COLUMN early_signals TEXT NOT NULL DEFAULT '[]';
ALTER TABLE attempts ADD COLUMN early_near TEXT NOT NULL DEFAULT '[]';
",
    "
ALTER TABLE tasks ADD COLUMN journal_arm TEXT NOT NULL DEFAULT 'treatment';
",
    "
CREATE TABLE projects (
  name TEXT PRIMARY KEY,
  purpose TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  workflow TEXT,
  per_task_usd REAL,
  per_initiative_usd REAL,
  supervisor_model TEXT,
  supervisor_per_lineage INTEGER,
  protected_json TEXT
);
CREATE TABLE project_repos (
  project TEXT NOT NULL REFERENCES projects(name),
  repo TEXT NOT NULL,
  scope_json TEXT,
  PRIMARY KEY (project, repo)
);
CREATE TABLE initiatives (
  id INTEGER PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(name),
  outcome TEXT NOT NULL,
  budget_usd REAL,
  stop_after_same_rule INTEGER NOT NULL DEFAULT 3,
  created_at INTEGER NOT NULL,
  settled_at INTEGER
);
CREATE TABLE backlog (
  id INTEGER PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(name),
  text TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  done_at INTEGER
);
ALTER TABLE tasks ADD COLUMN project TEXT;
ALTER TABLE tasks ADD COLUMN initiative INTEGER;
",
    "
ALTER TABLE tasks ADD COLUMN provider TEXT NOT NULL DEFAULT 'anthropic';
ALTER TABLE attempts ADD COLUMN runner TEXT NOT NULL DEFAULT 'claude-cli';
ALTER TABLE attempts ADD COLUMN provider TEXT NOT NULL DEFAULT 'anthropic';
",
    "
ALTER TABLE projects ADD COLUMN role_providers_json TEXT;
",
    "
CREATE TABLE deploy_targets (
  project TEXT NOT NULL REFERENCES projects(name),
  name TEXT NOT NULL,
  repo TEXT NOT NULL,
  scope_json TEXT,
  method TEXT NOT NULL,
  args_json TEXT NOT NULL DEFAULT '{}',
  check_cmd TEXT NOT NULL,
  on_landing INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (project, name)
);
CREATE TABLE deploys (
  id INTEGER PRIMARY KEY,
  project TEXT NOT NULL,
  target TEXT NOT NULL,
  sha TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  check_ok INTEGER,
  check_output TEXT NOT NULL DEFAULT '',
  rolled_back_to TEXT,
  reason TEXT NOT NULL DEFAULT ''
);
CREATE INDEX deploys_project_target ON deploys(project, target, id);
",
    "
ALTER TABLE deploys ADD COLUMN task_id INTEGER;
",
    "
ALTER TABLE tasks ADD COLUMN question_to TEXT;
ALTER TABLE decisions ADD COLUMN answered_for TEXT;
",
    "
ALTER TABLE tasks ADD COLUMN explore_json TEXT NOT NULL DEFAULT '{}';
",
    "
ALTER TABLE deploy_targets ADD COLUMN smoke_url TEXT;
ALTER TABLE deploys ADD COLUMN smoke_ok INTEGER;
ALTER TABLE deploys ADD COLUMN smoke_json TEXT;
",
    "
CREATE TABLE task_churn (
  task_id INTEGER PRIMARY KEY REFERENCES tasks(id),
  added_lines INTEGER NOT NULL,
  churned_lines INTEGER NOT NULL,
  computed_at INTEGER NOT NULL
);
",
    // Before ctx::resolve_provider stopped the supervisor from inheriting
    // a task's provider (task 309), a supervisor attempt still recorded
    // the task's model in `inputs_json`, not its own; the provider column
    // is already right (the built-in default, "anthropic"), so these rows
    // read "supervisor anthropic qwen3-coder:30b" instead of the
    // supervisor's own model. Backfill it to the supervisor's default,
    // "opus" (`config::Supervisor::model`'s default) — the only value a
    // migration with no access to the operator's config can give.
    "
UPDATE attempts SET inputs_json = json_set(inputs_json, '$.model', 'opus')
WHERE step = 'supervisor' AND provider = 'anthropic';
",
    // Replaces the path-overlap follow-on cost: `task_repair_cost` caches,
    // per landed task, the git-line-overlap attribution total (the
    // REPAIRCOST column); `line_overlap_cache` caches the per-(T, L) pair
    // git computation it is built from, keyed by both landed commits so
    // it is only ever computed once.
    "
CREATE TABLE task_repair_cost (
  task_id INTEGER PRIMARY KEY REFERENCES tasks(id),
  repair_cost REAL NOT NULL,
  computed_at INTEGER NOT NULL
);
CREATE TABLE line_overlap_cache (
  t_sha TEXT NOT NULL,
  l_sha TEXT NOT NULL,
  overlap_lines INTEGER NOT NULL,
  removed_lines INTEGER NOT NULL,
  PRIMARY KEY (t_sha, l_sha)
);
",
    // The assess directive's own record: one row per landing that ran it
    // (see src/assess.rs), never read by a view or `forge stats` — a
    // fast proxy for a landed task's true cost, kept beside it rather than
    // folded into either.
    "
CREATE TABLE assessments (
  id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(id),
  score INTEGER NOT NULL,
  findings_json TEXT NOT NULL DEFAULT '[]',
  model TEXT NOT NULL,
  provider TEXT NOT NULL,
  cost_usd REAL,
  created_at INTEGER NOT NULL
);
CREATE INDEX assessments_task ON assessments(task_id, id);
",
];

/// Width of the delayed-cost window: how long after a task lands a later
/// task's cost or a later landing's rewrite still counts against it (see
/// docs/LATER.md, "Defect escape" and the delayed-cost follow-up).
pub const THIRTY_DAYS_SECS: i64 = 30 * 86400;

/// The version this migration brings the schema to; `migrate` also runs
/// `seed_projects_from_tasks` in Rust when it applies this entry, since
/// naming a project after the Forge repository itself needs a filesystem
/// check no SQL string can express. Keep in sync with its position above.
const PROJECTS_MIGRATION_VERSION: i64 = 28;

/// The version this migration brings the schema to (the supervisor-model
/// backfill, see its comment above): fixed regardless of how many
/// migrations land after it, unlike "the last one", which is what the
/// regression test below needs to exclude exactly this migration from a
/// pre-fix fixture. Keep in sync with its position above. Test-only: no
/// production code needs to name this migration by version.
#[cfg(test)]
const SUPERVISOR_MODEL_BACKFILL_MIGRATION_VERSION: i64 = 36;

const TASK_COLUMNS: &[&str] = &[
    "id",
    "repo",
    "task",
    "base_branch",
    "base_sha",
    "branch",
    "worktree",
    "model",
    "provider",
    "max_turns",
    "max_attempts",
    "timeout_secs",
    "checks_json",
    "state",
    "reason",
    "question_to",
    "created_at",
    "started_at",
    "finished_at",
    "pushed",
    "worker_pid",
    "budget_usd",
    "worktree_removed_at",
    "allow_protected",
    "workflow",
    "interface",
    "show_checks",
    "workflow_hash",
    "workflow_text",
    "actions_json",
    "land",
    "after_json",
    "verify_base",
    "retry_of",
    "journal",
    "context",
    "context_enabled",
    "resume_on_failure",
    "plan",
    "landed_sha",
    "journal_arm",
    "project",
    "initiative",
    "explore_json",
];

fn conv<T, E: std::error::Error + Send + Sync + 'static>(
    r: &Row,
    name: &str,
    res: std::result::Result<T, E>,
) -> rusqlite::Result<T> {
    res.map_err(|e| {
        let idx = r.as_ref().column_index(name).unwrap_or(0);
        rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e))
    })
}

fn task_from_row(r: &Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get("id")?,
        repo: r.get("repo")?,
        task: r.get("task")?,
        base_branch: r.get("base_branch")?,
        base_sha: r.get("base_sha")?,
        branch: r.get("branch")?,
        worktree: r.get("worktree")?,
        model: r.get("model")?,
        provider: r.get("provider")?,
        max_turns: r.get("max_turns")?,
        max_attempts: r.get("max_attempts")?,
        timeout_secs: r.get("timeout_secs")?,
        checks: conv(
            r,
            "checks_json",
            serde_json::from_str(&r.get::<_, String>("checks_json")?),
        )?,
        state: conv(
            r,
            "state",
            TaskState::try_from(r.get::<_, String>("state")?.as_str()),
        )?,
        reason: r.get("reason")?,
        question_to: r.get("question_to")?,
        created_at: r.get("created_at")?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
        pushed: r.get::<_, i64>("pushed")? != 0,
        worker_pid: r.get("worker_pid")?,
        budget_usd: r.get("budget_usd")?,
        worktree_removed_at: r.get("worktree_removed_at")?,
        allow_protected: r.get::<_, i64>("allow_protected")? != 0,
        workflow: r.get("workflow")?,
        interface: r.get("interface")?,
        show_checks: r.get::<_, i64>("show_checks")? != 0,
        workflow_hash: r.get("workflow_hash")?,
        workflow_text: r.get("workflow_text")?,
        actions_json: r.get("actions_json")?,
        land: r.get::<_, i64>("land")? != 0,
        after: serde_json::from_str(&r.get::<_, String>("after_json")?).unwrap_or_default(),
        verify_base: r.get("verify_base")?,
        retry_of: r.get("retry_of")?,
        journal: r.get::<_, i64>("journal")? != 0,
        journal_arm: r.get("journal_arm")?,
        context: r.get("context")?,
        context_enabled: r.get::<_, i64>("context_enabled")? != 0,
        resume_on_failure: r.get::<_, i64>("resume_on_failure")? != 0,
        plan: r.get("plan")?,
        landed_sha: r.get("landed_sha")?,
        project: r.get("project")?,
        initiative: r.get("initiative")?,
        explore: conv(
            r,
            "explore_json",
            serde_json::from_str(&r.get::<_, String>("explore_json")?),
        )?,
    })
}

const ATTEMPT_COLUMNS: &[&str] = &[
    "id",
    "task_id",
    "attempt_no",
    "state",
    "reason",
    "started_at",
    "finished_at",
    "agent_exit",
    "timed_out",
    "num_turns",
    "tool_calls",
    "cost_usd",
    "agent_ms",
    "commits",
    "files_changed",
    "dirty",
    "verdict_json",
    "result_text",
    "log_path",
    "envelope_json",
    "rl_five_hour",
    "rl_seven_day",
    "rl_five_hour_resets",
    "rl_seven_day_resets",
    "step",
    "start_sha",
    "end_sha",
    "inputs_json",
    "outputs_json",
    "step_seq",
    "session_id",
    "first_edit",
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
    "early_signals",
    "early_near",
    "runner",
    "provider",
];

fn attempt_from_row(r: &Row) -> rusqlite::Result<Attempt> {
    Ok(Attempt {
        id: r.get("id")?,
        task_id: r.get("task_id")?,
        attempt_no: r.get("attempt_no")?,
        state: conv(
            r,
            "state",
            AttemptState::try_from(r.get::<_, String>("state")?.as_str()),
        )?,
        reason: r.get("reason")?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
        agent_exit: r.get("agent_exit")?,
        timed_out: r.get::<_, i64>("timed_out")? != 0,
        num_turns: r.get("num_turns")?,
        tool_calls: r.get("tool_calls")?,
        cost_usd: r.get("cost_usd")?,
        agent_ms: r.get("agent_ms")?,
        commits: r.get("commits")?,
        files_changed: r.get("files_changed")?,
        dirty: r.get::<_, i64>("dirty")? != 0,
        verdict_json: r.get("verdict_json")?,
        result_text: r.get("result_text")?,
        log_path: r.get("log_path")?,
        envelope_json: r.get("envelope_json")?,
        rl_five_hour: r.get("rl_five_hour")?,
        rl_seven_day: r.get("rl_seven_day")?,
        rl_five_hour_resets: r.get("rl_five_hour_resets")?,
        rl_seven_day_resets: r.get("rl_seven_day_resets")?,
        step: r.get("step")?,
        start_sha: r.get("start_sha")?,
        end_sha: r.get("end_sha")?,
        inputs_json: r.get("inputs_json")?,
        outputs_json: r.get("outputs_json")?,
        step_seq: r.get("step_seq")?,
        session_id: r.get("session_id")?,
        first_edit: r.get("first_edit")?,
        input_tokens: r.get("input_tokens")?,
        output_tokens: r.get("output_tokens")?,
        cache_read_input_tokens: r.get("cache_read_input_tokens")?,
        cache_creation_input_tokens: r.get("cache_creation_input_tokens")?,
        early_signals: r.get("early_signals")?,
        early_near: r.get("early_near")?,
        runner: r.get("runner")?,
        provider: r.get("provider")?,
    })
}

/// `id` and every task it retries, walking up through `retry_of` to the root.
fn lineage_ids(conn: &Connection, id: i64) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE up(id, parent) AS (
           SELECT id, retry_of FROM tasks WHERE id = ?1
           UNION ALL SELECT t.id, t.retry_of FROM up JOIN tasks t ON t.id = up.parent)
         SELECT id FROM up",
    )?;
    let rows = stmt.query_map(params![id], |r| r.get(0))?;
    rows.collect()
}

/// `task_repair_cost`'s cached row for `task_id`: `(repair_cost,
/// computed_at)`, or `None` if it has never been computed. See
/// `compute_repair_cost` in view.rs for how the git-level number is
/// derived.
fn repair_cost_cache_query(c: &Connection, task_id: i64) -> Result<Option<(f64, i64)>> {
    Ok(c.query_row(
        "SELECT repair_cost, computed_at FROM task_repair_cost WHERE task_id = ?1",
        params![task_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

fn set_repair_cost_cache_query(
    c: &Connection,
    task_id: i64,
    repair_cost: f64,
    computed_at: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO task_repair_cost (task_id, repair_cost, computed_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(task_id) DO UPDATE SET
           repair_cost = excluded.repair_cost,
           computed_at = excluded.computed_at",
        params![task_id, repair_cost, computed_at],
    )?;
    Ok(())
}

/// The cached line-overlap between an earlier landing (`t_sha`, the
/// landed commit whose lines were added) and a later one (`l_sha`, the
/// landed commit that removed or rewrote lines): `(overlap_lines,
/// removed_lines)`, where `overlap_lines` is how many lines `t_sha`'s
/// landing added that `l_sha`'s landing removed or rewrote, and
/// `removed_lines` is how many lines `l_sha`'s landing removed or rewrote
/// in total. Keyed by both landed commits, not task ids, since the
/// underlying git diffs never change once a task has landed.
fn line_overlap_cache_query(
    c: &Connection,
    t_sha: &str,
    l_sha: &str,
) -> Result<Option<(i64, i64)>> {
    Ok(c.query_row(
        "SELECT overlap_lines, removed_lines FROM line_overlap_cache WHERE t_sha = ?1 AND l_sha = ?2",
        params![t_sha, l_sha],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

fn set_line_overlap_cache_query(
    c: &Connection,
    t_sha: &str,
    l_sha: &str,
    overlap_lines: i64,
    removed_lines: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO line_overlap_cache (t_sha, l_sha, overlap_lines, removed_lines)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(t_sha, l_sha) DO UPDATE SET
           overlap_lines = excluded.overlap_lines,
           removed_lines = excluded.removed_lines",
        params![t_sha, l_sha, overlap_lines, removed_lines],
    )?;
    Ok(())
}

/// The (provider, model) pairs `task_id` ran a `code` attempt under: which
/// `by_role` groups a landed task's delayed cost is attributed to.
fn code_attempt_groups_query(c: &Connection, task_id: i64) -> Result<Vec<(String, String)>> {
    let mut stmt = c.prepare(
        "SELECT DISTINCT provider, COALESCE(json_extract(inputs_json, '$.model'), '')
         FROM attempts WHERE task_id = ?1 AND step = 'code'",
    )?;
    let rows = stmt.query_map(params![task_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `task_churn`'s cached row for `task_id`: `(added_lines, churned_lines,
/// computed_at)`, or `None` if it has never been computed.
fn churn_cache_query(c: &Connection, task_id: i64) -> Result<Option<(i64, i64, i64)>> {
    Ok(c.query_row(
        "SELECT added_lines, churned_lines, computed_at FROM task_churn WHERE task_id = ?1",
        params![task_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()?)
}

fn set_churn_cache_query(
    c: &Connection,
    task_id: i64,
    added_lines: i64,
    churned_lines: i64,
    computed_at: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO task_churn (task_id, added_lines, churned_lines, computed_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(task_id) DO UPDATE SET
           added_lines = excluded.added_lines,
           churned_lines = excluded.churned_lines,
           computed_at = excluded.computed_at",
        params![task_id, added_lines, churned_lines, computed_at],
    )?;
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;",
        )?;
        migrate(&conn)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .lock()
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    pub fn insert_task(&self, t: &Task) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO tasks (repo, task, base_branch, model, provider, max_turns, max_attempts, timeout_secs, checks_json,
                                state, created_at, budget_usd, allow_protected, workflow, show_checks, workflow_hash, workflow_text, land, after_json, retry_of, journal, context_enabled, resume_on_failure, journal_arm, explore_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
            params![
                t.repo,
                t.task,
                t.base_branch,
                t.model,
                t.provider,
                t.max_turns,
                t.max_attempts,
                t.timeout_secs,
                serde_json::to_string(&t.checks)?,
                t.state.as_str(),
                t.created_at,
                t.budget_usd,
                t.allow_protected as i64,
                t.workflow,
                t.show_checks as i64,
                t.workflow_hash,
                t.workflow_text,
                t.land as i64,
                serde_json::to_string(&t.after)?,
                t.retry_of,
                t.journal as i64,
                t.context_enabled as i64,
                t.resume_on_failure as i64,
                t.journal_arm,
                serde_json::to_string(&t.explore)?,
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Persist every column the struct carries, except the id, the
    /// creation time, and `worktree_removed_at`, which gc owns. A field
    /// mutated after insert used to be silently dropped here.
    pub fn update_task(&self, t: &Task) -> Result<()> {
        self.lock().execute(
            "UPDATE tasks SET repo=?2, task=?3, base_branch=?4, base_sha=?5, branch=?6, worktree=?7, model=?8,
             max_turns=?9, max_attempts=?10, timeout_secs=?11, checks_json=?12, state=?13, reason=?14,
             started_at=?15, finished_at=?16, pushed=?17, worker_pid=?18, budget_usd=?19, allow_protected=?20,
             workflow=?21, workflow_hash=?22, workflow_text=?23, actions_json=?24, interface=?25, show_checks=?26,
             land=?27, after_json=?28, verify_base=?29, retry_of=?30, journal=?31, context=?32,
             context_enabled=?33, resume_on_failure=?34, plan=?35, landed_sha=?36, journal_arm=?37,
             project=?38, initiative=?39, provider=?40, question_to=?41, explore_json=?42 WHERE id=?1",
            params![
                t.id,
                t.repo,
                t.task,
                t.base_branch,
                t.base_sha,
                t.branch,
                t.worktree,
                t.model,
                t.max_turns,
                t.max_attempts,
                t.timeout_secs,
                serde_json::to_string(&t.checks)?,
                t.state.as_str(),
                t.reason,
                t.started_at,
                t.finished_at,
                t.pushed as i64,
                t.worker_pid,
                t.budget_usd,
                t.allow_protected as i64,
                t.workflow,
                t.workflow_hash,
                t.workflow_text,
                t.actions_json,
                t.interface,
                t.show_checks as i64,
                t.land as i64,
                serde_json::to_string(&t.after)?,
                t.verify_base,
                t.retry_of,
                t.journal as i64,
                t.context,
                t.context_enabled as i64,
                t.resume_on_failure as i64,
                t.plan,
                t.landed_sha,
                t.journal_arm,
                t.project,
                t.initiative,
                t.provider,
                t.question_to,
                serde_json::to_string(&t.explore)?,
            ],
        )?;
        Ok(())
    }

    pub fn task(&self, id: i64) -> Result<Option<Task>> {
        Ok(self
            .lock()
            .query_row(
                &format!("SELECT {} FROM tasks WHERE id=?1", TASK_COLUMNS.join(", ")),
                params![id],
                task_from_row,
            )
            .optional()?)
    }

    /// Queued tasks whose dependencies have all landed (or succeeded
    /// without landing, when they were told not to) and whose initiative
    /// is not in `held`, oldest first: what `claim_next` considers.
    pub fn queued_unblocked(&self, held: &[i64]) -> Result<Vec<Task>> {
        let ids: Vec<i64> = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT t.id FROM tasks t WHERE t.state='queued' AND NOT EXISTS (
                   SELECT 1 FROM json_each(t.after_json) j LEFT JOIN tasks d ON d.id = j.value
                   WHERE d.id IS NULL OR d.state != 'succeeded' OR (d.land = 1 AND d.landed_sha = '')
                 ) ORDER BY t.id",
            )?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut out = Vec::new();
        for id in ids {
            let Some(t) = self.task(id)? else { continue };
            if !t.initiative.is_some_and(|i| held.contains(&i)) {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// Atomically take the oldest queued task for this worker, skipping
    /// any whose initiative is in `held` (the caller has already found
    /// those initiatives are holding new claims, see
    /// `view::initiative_hold`) or for which `provider_held` says the
    /// provider it would run under is at its rate-window cap: the oldest
    /// queued, unheld task whose dependencies have all landed.
    pub fn claim_next(
        &self,
        pid: i64,
        held: &[i64],
        provider_held: impl Fn(&Task) -> bool,
    ) -> Result<Option<Task>> {
        for t in self.queued_unblocked(held)? {
            if provider_held(&t) {
                continue;
            }
            if self.claim(t.id, pid)? {
                return self.task(t.id);
            }
        }
        Ok(None)
    }

    /// Atomically take one specific queued task.
    pub fn claim(&self, id: i64, pid: i64) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE tasks SET state='running', worker_pid=?2, started_at=?3 WHERE id=?1 AND state='queued'",
            params![id, pid, crate::unix_now()],
        )?;
        Ok(n == 1)
    }

    /// Withdraw a blocked or queued task: the operator decided it should
    /// not be done. Atomic on state, so a task the worker claims in
    /// between is left alone. Returns whether it changed anything.
    pub fn withdraw(&self, id: i64, reason: &str) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE tasks SET state='withdrawn', reason=?2, finished_at=?3 WHERE id=?1 AND state IN ('blocked', 'queued')",
            params![id, reason, crate::unix_now()],
        )?;
        Ok(n == 1)
    }

    /// Block every queued task that waits on a task which ended without
    /// landing. Returns the (dependent, dependency) pairs it blocked.
    pub fn block_dependents(&self) -> Result<Vec<(i64, i64, String)>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.id, d.id, d.state, d.reason FROM tasks t, json_each(t.after_json) j JOIN tasks d ON d.id = j.value
             WHERE t.state='queued' AND d.state IN ('failed', 'unverified', 'withdrawn')
                OR (t.state='queued' AND d.state='succeeded' AND d.land = 1 AND d.landed_sha = '' AND d.finished_at IS NOT NULL)
             ORDER BY t.id, d.id",
        )?;
        let rows: Vec<(i64, i64, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = Vec::new();
        for (t, d, state, reason) in rows {
            let why = format!("waits on task {d} ({state}: {reason})");
            let n = c.execute(
                "UPDATE tasks SET state='blocked', reason=?2, finished_at=?3 WHERE id=?1 AND state='queued'",
                params![t, why, crate::unix_now()],
            )?;
            if n > 0 {
                out.push((t, d, why));
            }
        }
        Ok(out)
    }

    /// A retry of `old` carries its dependents along: every task waiting
    /// on `old` waits on `new` instead, and one that was swept into
    /// blocked because `old` ended is queued again. Returns the ids moved.
    pub fn reroute_dependents(&self, old: i64, new: i64) -> Result<Vec<i64>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.id, t.after_json, t.state, t.reason FROM tasks t, json_each(t.after_json) j
             WHERE j.value = ?1 AND t.state IN ('queued', 'blocked') AND t.id != ?2",
        )?;
        let rows: Vec<(i64, String, String, String)> = stmt
            .query_map(params![old, new], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut moved = Vec::new();
        for (id, after_json, state, reason) in rows {
            let after: Vec<i64> = serde_json::from_str(&after_json).unwrap_or_default();
            let after: Vec<i64> = after
                .into_iter()
                .map(|d| if d == old { new } else { d })
                .collect();
            let swept = state == "blocked" && reason.starts_with(&format!("waits on task {old} "));
            if swept {
                c.execute(
                    "UPDATE tasks SET after_json=?2, state='queued', reason='', finished_at=NULL WHERE id=?1",
                    params![id, serde_json::to_string(&after)?],
                )?;
            } else {
                c.execute(
                    "UPDATE tasks SET after_json=?2 WHERE id=?1",
                    params![id, serde_json::to_string(&after)?],
                )?;
            }
            moved.push(id);
        }
        Ok(moved)
    }

    /// The first task in `id`'s chain of retries: itself when it retries nothing.
    pub fn root_of(&self, id: i64) -> Result<i64> {
        // Retries always point at an already-existing task, so ids only
        // shrink walking up the chain: the root is the smallest one.
        Ok(lineage_ids(&self.lock(), id)?.into_iter().min().unwrap())
    }

    /// Every task in `id`'s lineage, root first: the root and everything
    /// that retries it, directly or through other retries.
    pub fn lineage(&self, id: i64) -> Result<Vec<LineageRow>> {
        let root = self.root_of(id)?;
        let c = self.lock();
        let mut stmt = c.prepare(
            "WITH RECURSIVE down(id) AS (
               SELECT ?1 UNION ALL SELECT t.id FROM down JOIN tasks t ON t.retry_of = down.id)
             SELECT t.id, t.retry_of, t.state, t.reason, t.workflow,
                    COALESCE((SELECT SUM(cost_usd) FROM attempts a WHERE a.task_id = t.id), 0)
             FROM down JOIN tasks t ON t.id = down.id ORDER BY t.id",
        )?;
        let rows = stmt.query_map(params![root], |r| {
            Ok(LineageRow {
                id: r.get(0)?,
                parent: r.get(1)?,
                state: r.get(2)?,
                reason: r.get(3)?,
                workflow: r.get(4)?,
                cost: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every task that retries `id` directly.
    pub fn dependents_retries(&self, id: i64) -> Result<Vec<i64>> {
        let c = self.lock();
        let mut stmt = c.prepare("SELECT id FROM tasks WHERE retry_of=?1 ORDER BY id")?;
        let rows = stmt.query_map(params![id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The newest task that retries `id`, if any.
    pub fn latest_retry_of(&self, id: i64) -> Result<Option<i64>> {
        Ok(self.lock().query_row(
            "SELECT MAX(id) FROM tasks WHERE retry_of=?1",
            params![id],
            |r| r.get::<_, Option<i64>>(0),
        )?)
    }

    pub fn queued_count(&self) -> Result<i64> {
        Ok(self
            .lock()
            .query_row("SELECT COUNT(*) FROM tasks WHERE state='queued'", [], |r| {
                r.get(0)
            })?)
    }

    /// Put a running task back in the queue, closing its open attempt as
    /// agent_failed with `why`, so the next worker resumes at the following
    /// attempt number.
    pub fn requeue(&self, id: i64, why: &str) -> Result<()> {
        let c = self.lock();
        c.execute(
            "UPDATE attempts SET state='agent_failed', reason=?2, finished_at=?3 WHERE task_id=?1 AND state='running'",
            params![id, why, crate::unix_now()],
        )?;
        c.execute(
            "UPDATE tasks SET state='queued', worker_pid=NULL, reason=?2 WHERE id=?1 AND state='running'",
            params![id, format!("requeued: {why}")],
        )?;
        Ok(())
    }

    /// Tasks left in `running` by a worker that no longer exists.
    pub fn orphans(&self, alive: impl Fn(i64) -> bool) -> Result<Vec<i64>> {
        let c = self.lock();
        let mut stmt = c.prepare("SELECT id, worker_pid FROM tasks WHERE state='running'")?;
        let running: Vec<(i64, Option<i64>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(running
            .into_iter()
            .filter(|(_, pid)| !pid.is_some_and(&alive))
            .map(|(id, _)| id)
            .collect())
    }

    pub fn insert_attempt(&self, a: &Attempt) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO attempts (task_id, attempt_no, state, started_at, log_path, step, start_sha, inputs_json, step_seq, runner, provider)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![a.task_id, a.attempt_no, a.state.as_str(), a.started_at, a.log_path, a.step, a.start_sha, a.inputs_json, a.step_seq, a.runner, a.provider],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn finish_attempt(&self, a: &FinishAttempt) -> Result<()> {
        self.lock().execute(
            "UPDATE attempts SET state=?2, reason=?3, finished_at=?4, agent_exit=?5, timed_out=?6, num_turns=?7,
             tool_calls=?8, cost_usd=?9, agent_ms=?10, commits=?11, files_changed=?12, dirty=?13, verdict_json=?14,
             result_text=?15, envelope_json=?16, rl_five_hour=?17, rl_seven_day=?18, rl_five_hour_resets=?19,
             rl_seven_day_resets=?20, end_sha=?21, outputs_json=?22, session_id=?23, first_edit=?24,
             input_tokens=?25, output_tokens=?26, cache_read_input_tokens=?27, cache_creation_input_tokens=?28,
             early_signals=?29, early_near=?30 WHERE id=?1",
            params![
                a.id,
                a.state.as_str(),
                a.reason,
                a.finished_at,
                a.agent_exit,
                a.timed_out as i64,
                a.num_turns,
                a.tool_calls,
                a.cost_usd,
                a.agent_ms,
                a.commits,
                a.files_changed,
                a.dirty as i64,
                a.verdict_json,
                a.result_text,
                a.envelope_json,
                a.rl_five_hour,
                a.rl_seven_day,
                a.rl_five_hour_resets,
                a.rl_seven_day_resets,
                a.end_sha,
                a.outputs_json,
                a.session_id,
                a.first_edit,
                a.input_tokens,
                a.output_tokens,
                a.cache_read_input_tokens,
                a.cache_creation_input_tokens,
                a.early_signals,
                a.early_near,
            ],
        )?;
        Ok(())
    }

    pub fn running_ids(&self) -> Result<Vec<i64>> {
        let c = self.lock();
        let mut stmt = c.prepare("SELECT id FROM tasks WHERE state='running' ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The most recent rate-limit sample any attempt on `provider` recorded:
    /// the window hold is per provider, since each has its own subscription
    /// (or none at all).
    pub fn latest_rate_limit(&self, provider: &str) -> Result<Option<RateLimitSample>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT COALESCE(finished_at, started_at), rl_five_hour, rl_seven_day, rl_five_hour_resets, rl_seven_day_resets FROM attempts
                 WHERE provider = ?1 AND (rl_five_hour IS NOT NULL OR rl_seven_day IS NOT NULL) ORDER BY id DESC LIMIT 1",
                params![provider],
                |r| Ok(RateLimitSample { seen_at: r.get(0)?, five_hour: r.get(1)?, seven_day: r.get(2)?, five_hour_resets: r.get(3)?, seven_day_resets: r.get(4)? }),
            )
            .optional()?)
    }

    pub fn insert_op(&self, o: &Op) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO ops (task_id, seq, name, kernel, started_at, ms, ok, exit, detail, attempt_id, output)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![o.task_id, o.seq, o.name, o.kernel as i64, o.started_at, o.ms, o.ok as i64, o.exit, o.detail, o.attempt_id, o.output],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn ops(&self, task_id: i64) -> Result<Vec<Op>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT id, task_id, seq, name, kernel, started_at, ms, ok, exit, detail, attempt_id, output FROM ops WHERE task_id=?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![task_id], |r| {
            Ok(Op {
                id: r.get(0)?,
                task_id: r.get(1)?,
                seq: r.get(2)?,
                name: r.get(3)?,
                kernel: r.get::<_, i64>(4)? != 0,
                started_at: r.get(5)?,
                ms: r.get(6)?,
                ok: r.get::<_, i64>(7)? != 0,
                exit: r.get(8)?,
                detail: r.get(9)?,
                attempt_id: r.get(10)?,
                output: r.get(11)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn attempts(&self, task_id: i64) -> Result<Vec<Attempt>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM attempts WHERE task_id=?1 ORDER BY attempt_no",
            ATTEMPT_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![task_id], attempt_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// (task_id, step, outputs_json) for every attempt, in one query, so
    /// callers can pull tool facts out of outputs_json without an N+1 over
    /// tasks. Optionally restricted to a single step.
    pub fn attempt_tool_facts(&self, step: Option<&str>) -> Result<Vec<(i64, String, String)>> {
        let c = self.lock();
        let rows = match step {
            Some(step) => {
                let mut stmt =
                    c.prepare("SELECT task_id, step, outputs_json FROM attempts WHERE step=?1")?;
                let rows =
                    stmt.query_map(params![step], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => {
                let mut stmt = c.prepare("SELECT task_id, step, outputs_json FROM attempts")?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        Ok(rows)
    }

    /// Total cost of a task's attempts so far, from the CLI's accounting.
    pub fn task_cost(&self, task_id: i64) -> Result<f64> {
        Ok(self.lock().query_row(
            "SELECT COALESCE(SUM(cost_usd), 0) FROM attempts WHERE task_id=?1",
            params![task_id],
            |r| r.get(0),
        )?)
    }

    /// Cost of every attempt started at or after `since`.
    /// The files successful attempts on this repository read most: a prior
    /// for where a new task's answer is likely to be. From the tool facts.
    pub fn hot_files(&self, repo: &str, n: usize) -> Result<Vec<String>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT a.outputs_json FROM attempts a JOIN tasks t ON t.id = a.task_id
             WHERE t.repo = ?1 AND a.state = 'succeeded' AND a.step != 'review'",
        )?;
        let rows: Vec<String> = stmt
            .query_map(params![repo], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for json in rows {
            if let Ok(o) = serde_json::from_str::<crate::audit::Outputs>(&json)
                && let Some(t) = o.tools
            {
                for (path, k) in t.reads {
                    if !path.starts_with('/') {
                        *counts.entry(path).or_default() += k;
                    }
                }
            }
        }
        let mut v: Vec<(String, u64)> = counts.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(v.into_iter().take(n).map(|(p, _)| p).collect())
    }

    pub fn spent_since(&self, since: i64) -> Result<f64> {
        Ok(self.lock().query_row(
            "SELECT COALESCE(SUM(cost_usd), 0) FROM attempts WHERE started_at >= ?1",
            params![since],
            |r| r.get(0),
        )?)
    }

    /// How many `interview` attempts blocked on a question since `since`:
    /// the operator's `[intake] max_questions_per_day` cap, one row per
    /// person-facing turn (the confirmation counts as one).
    pub fn interview_questions_since(&self, since: i64) -> Result<i64> {
        Ok(self.lock().query_row(
            "SELECT COUNT(*) FROM attempts WHERE step = 'interview' AND state = 'needs_input' AND started_at >= ?1",
            params![since],
            |r| r.get(0),
        )?)
    }

    /// Tasks whose worktree is still on disk as far as Forge knows.
    pub fn tasks_with_worktrees(&self) -> Result<Vec<Task>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM tasks WHERE worktree != '' AND worktree_removed_at IS NULL ORDER BY id",
            TASK_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map([], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Landed tasks, scoped like every other `forge stats` query: the
    /// input to both delayed-cost signals (the `task_repair_cost` cache
    /// and the `task_churn` cache).
    pub fn landed_tasks(&self, scope: &StatsFilter) -> Result<Vec<Task>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM tasks WHERE landed_sha != ''
               AND (?1 IS NULL OR project = ?1) AND (?2 IS NULL OR initiative = ?2)
             ORDER BY id",
            TASK_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![scope.project, scope.initiative], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Landed tasks on `repo`, other than `exclude`, whose landing fell in
    /// `(from, to]`: the later landings a churn computation diffs `added`
    /// against (see docs/LATER.md, the delayed-cost follow-up to "Defect
    /// escape").
    pub fn later_landings(
        &self,
        repo: &str,
        exclude: i64,
        from: i64,
        to: i64,
    ) -> Result<Vec<Task>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM tasks WHERE repo = ?1 AND id != ?2 AND landed_sha != ''
               AND finished_at > ?3 AND finished_at <= ?4
             ORDER BY id",
            TASK_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![repo, exclude, from, to], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// `task_churn`'s cached row for `task_id`, if it has been computed.
    pub fn churn_cache(&self, task_id: i64) -> Result<Option<(i64, i64, i64)>> {
        churn_cache_query(&self.lock(), task_id)
    }

    /// Write (or overwrite) `task_id`'s cached churn: lines it added and,
    /// of those, how many a later landing removed or rewrote within the
    /// window, as of `computed_at`.
    pub fn set_churn_cache(
        &self,
        task_id: i64,
        added_lines: i64,
        churned_lines: i64,
        computed_at: i64,
    ) -> Result<()> {
        set_churn_cache_query(
            &self.lock(),
            task_id,
            added_lines,
            churned_lines,
            computed_at,
        )
    }

    /// `task_repair_cost`'s cached row for `task_id`, if it has been
    /// computed.
    pub fn repair_cost_cache(&self, task_id: i64) -> Result<Option<(f64, i64)>> {
        repair_cost_cache_query(&self.lock(), task_id)
    }

    /// Write (or overwrite) `task_id`'s cached repair cost, as of
    /// `computed_at`.
    pub fn set_repair_cost_cache(
        &self,
        task_id: i64,
        repair_cost: f64,
        computed_at: i64,
    ) -> Result<()> {
        set_repair_cost_cache_query(&self.lock(), task_id, repair_cost, computed_at)
    }

    /// The cached line-overlap between an earlier landing (`t_sha`) and a
    /// later one (`l_sha`), if it has been computed.
    pub fn line_overlap_cache(&self, t_sha: &str, l_sha: &str) -> Result<Option<(i64, i64)>> {
        line_overlap_cache_query(&self.lock(), t_sha, l_sha)
    }

    /// Write (or overwrite) the cached line-overlap between `t_sha` and
    /// `l_sha`.
    pub fn set_line_overlap_cache(
        &self,
        t_sha: &str,
        l_sha: &str,
        overlap_lines: i64,
        removed_lines: i64,
    ) -> Result<()> {
        set_line_overlap_cache_query(&self.lock(), t_sha, l_sha, overlap_lines, removed_lines)
    }

    pub fn mark_worktree_removed(&self, id: i64) -> Result<()> {
        self.lock().execute(
            "UPDATE tasks SET worktree_removed_at=?2 WHERE id=?1",
            params![id, crate::unix_now()],
        )?;
        Ok(())
    }

    /// Blocked tasks: the demand signal for workflows and the questions
    /// waiting on the operator.
    pub fn blocked(&self, repo: Option<&str>) -> Result<Vec<Task>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM tasks t WHERE t.state='blocked'
               AND (?1 IS NULL OR t.repo = ?1)
               AND NOT EXISTS (SELECT 1 FROM tasks n WHERE n.retry_of = t.id) ORDER BY t.id",
            TASK_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![repo], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The most recent terminal tasks of a workflow, newest first, for a
    /// profile. `hash` narrows to one version.
    pub fn runs(
        &self,
        workflow: &str,
        hash: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::profile::Run>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.id, t.state, COALESCE((SELECT SUM(cost_usd) FROM attempts a WHERE a.task_id=t.id),0),
                    COALESCE(t.finished_at - t.started_at, 0),
                    (SELECT COUNT(*) FROM attempts a WHERE a.task_id=t.id)
             FROM tasks t WHERE t.workflow=?1 AND (?2 IS NULL OR t.workflow_hash=?2)
               AND t.state IN ('succeeded','failed','blocked','unverified')
               AND t.started_at IS NOT NULL
             ORDER BY t.id DESC LIMIT ?3",
        )?;
        let rows: Vec<(i64, String, f64, i64, i64)> = stmt
            .query_map(params![workflow, hash, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, state, cost, secs, attempts) in rows {
            let root = lineage_ids(&c, id)?.into_iter().min().unwrap();
            out.push(crate::profile::Run {
                succeeded: state == "succeeded",
                cost,
                secs: secs as f64,
                attempts,
                root,
            });
        }
        Ok(out)
    }

    /// Workflow versions seen, newest first, by the id of the last task that ran them.
    pub fn workflow_versions(&self, workflow: &str) -> Result<Vec<String>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT workflow_hash FROM tasks WHERE workflow=?1 AND workflow_hash != '' GROUP BY workflow_hash ORDER BY MAX(id) DESC",
        )?;
        let rows = stmt.query_map(params![workflow], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Outcomes per workflow version: the table that compares workflows.
    pub fn workflow_stats(&self, scope: &StatsFilter) -> Result<Vec<WorkflowStat>> {
        let mut stats = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT t.workflow, t.workflow_hash, COUNT(*),
                    SUM(t.state='succeeded'), SUM(t.state='failed'), SUM(t.state='blocked'), SUM(t.state='unverified'),
                    COALESCE((SELECT SUM(a.cost_usd) FROM attempts a WHERE a.task_id IN (
                        SELECT id FROM tasks t2 WHERE t2.workflow=t.workflow AND t2.workflow_hash=t.workflow_hash
                          AND (?1 IS NULL OR t2.project = ?1) AND (?2 IS NULL OR t2.initiative = ?2)
                    )), 0),
                    COALESCE((SELECT COUNT(*) FROM attempts a WHERE a.task_id IN (
                        SELECT id FROM tasks t2 WHERE t2.workflow=t.workflow AND t2.workflow_hash=t.workflow_hash
                          AND (?1 IS NULL OR t2.project = ?1) AND (?2 IS NULL OR t2.initiative = ?2)
                    )), 0),
                    SUM(t.landed_sha != ''),
                    SUM(t.landed_sha != '' AND EXISTS (
                        SELECT 1 FROM attempts a
                        JOIN tasks b ON b.id = a.task_id
                        WHERE b.base_sha = t.landed_sha
                          AND a.step = 'code'
                          AND a.attempt_no = (SELECT MIN(a2.attempt_no) FROM attempts a2 WHERE a2.task_id = a.task_id AND a2.step = 'code')
                          AND EXISTS (
                              SELECT 1 FROM json_each(a.verdict_json) j
                              WHERE json_extract(j.value, '$.level') = 'L1' AND json_extract(j.value, '$.ok') = 0
                          )
                    )),
                    SUM(t.landed_sha != '' AND EXISTS (
                        SELECT 1 FROM task_refs r WHERE r.kind = 'repairs' AND r.url = 'forge://task/' || t.id
                    ))
             FROM tasks t WHERE t.state IN ('succeeded','failed','blocked','unverified') AND t.started_at IS NOT NULL
               AND (?1 IS NULL OR t.project = ?1) AND (?2 IS NULL OR t.initiative = ?2)
             GROUP BY t.workflow, t.workflow_hash ORDER BY t.workflow, t.workflow_hash",
            )?;
            let rows = stmt.query_map(params![scope.project, scope.initiative], |r| {
                Ok(WorkflowStat {
                    workflow: r.get(0)?,
                    hash: r.get(1)?,
                    tasks: r.get(2)?,
                    succeeded: r.get(3)?,
                    failed: r.get(4)?,
                    blocked: r.get(5)?,
                    unverified: r.get(6)?,
                    cost: r.get(7)?,
                    attempts: r.get(8)?,
                    landed: r.get(9)?,
                    broke_base: r.get(10)?,
                    repaired: r.get(11)?,
                    repair_cost: 0.0,
                    added_lines: 0,
                    churned_lines: 0,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<WorkflowStat>>>()?
        };
        let landed = self.landed_tasks(scope)?;
        let c = self.lock();
        for t in &landed {
            let Some(w) = stats
                .iter_mut()
                .find(|w| w.workflow == t.workflow && w.hash == t.workflow_hash)
            else {
                continue;
            };
            if let Some((cost, _)) = repair_cost_cache_query(&c, t.id)? {
                w.repair_cost += cost;
            }
            if let Some((added, churned, _)) = churn_cache_query(&c, t.id)? {
                w.added_lines += added;
                w.churned_lines += churned;
            }
        }
        Ok(stats)
    }

    /// Outcomes per workflow step.
    pub fn step_stats(&self, scope: &StatsFilter) -> Result<Vec<StepStat>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.workflow, a.step, COUNT(*), SUM(a.state='succeeded'), SUM(a.state='agent_failed'),
                    SUM(a.state='checks_failed'), SUM(a.state='needs_input'), AVG(a.num_turns), COALESCE(SUM(a.cost_usd),0), AVG(a.agent_ms),
                    AVG(a.first_edit), AVG(a.input_tokens)
             FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.state != 'running'
               AND (?1 IS NULL OR t.project = ?1) AND (?2 IS NULL OR t.initiative = ?2)
             GROUP BY t.workflow, a.step ORDER BY t.workflow, a.step",
        )?;
        let rows = stmt.query_map(params![scope.project, scope.initiative], |r| {
            Ok(StepStat {
                workflow: r.get(0)?,
                step: r.get(1)?,
                attempts: r.get(2)?,
                succeeded: r.get(3)?,
                agent_failed: r.get(4)?,
                checks_failed: r.get(5)?,
                needs_input: r.get(6)?,
                mean_turns: r.get(7)?,
                cost: r.get(8)?,
                mean_ms: r.get(9)?,
                mean_first_edit: r.get(10)?,
                mean_input_tokens: r.get(11)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The journal control arm's retrospective split, over code attempts
    /// after the first: one row for attempts handed a journal
    /// (`inputs_json`'s `journal` field present and non-empty), one for
    /// attempts that were not. A side with no matching attempts is omitted.
    pub fn journal_control_stats(&self) -> Result<Vec<JournalStat>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT
                json_extract(a.inputs_json, '$.journal') IS NOT NULL
                    AND json_extract(a.inputs_json, '$.journal') != '' AS has_journal,
                COUNT(*), SUM(a.state='succeeded'), AVG(a.num_turns), AVG(a.first_edit),
                COALESCE(AVG(a.cost_usd), 0)
             FROM attempts a
             WHERE a.step = 'code' AND a.attempt_no > 1 AND a.state != 'running'
             GROUP BY has_journal",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(JournalStat {
                has_journal: r.get(0)?,
                attempts: r.get(1)?,
                succeeded: r.get(2)?,
                mean_turns: r.get(3)?,
                mean_first_edit: r.get(4)?,
                mean_cost_usd: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The runner breakdown: attempts, outcomes, cost and wall time per
    /// (role, provider, model), role being the attempt's step. For the
    /// `code` role only, also the landed count and the broke-base count
    /// (see `WorkflowStat::broke_base`) for tasks with an attempt in the
    /// group. `investigate` and `interview` are read-only directives whose
    /// job is to ask when the record does not settle it; an attempt of
    /// either that ended `needs_input` with a plain question counts as a
    /// success here (see `forge stats --by-role`'s footnote).
    pub fn role_stats(&self) -> Result<Vec<RoleStat>> {
        let mut stats = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT a.step, a.provider, COALESCE(json_extract(a.inputs_json, '$.model'), '') AS attempt_model,
                    COUNT(*),
                    SUM(CASE
                        WHEN a.state='succeeded' THEN 1
                        WHEN a.step IN ('investigate', 'interview') AND a.state='needs_input'
                            AND a.envelope_json != '' AND json_valid(a.envelope_json)
                            AND COALESCE(json_extract(a.envelope_json, '$.needs_input.kind'), 'question') = 'question'
                        THEN 1
                        ELSE 0
                    END),
                    AVG(a.num_turns), COALESCE(AVG(a.cost_usd), 0), AVG(a.agent_ms),
                    COUNT(DISTINCT CASE WHEN t.landed_sha != '' THEN t.id END),
                    COUNT(DISTINCT CASE WHEN t.landed_sha != '' AND EXISTS (
                        SELECT 1 FROM attempts a2
                        JOIN tasks b ON b.id = a2.task_id
                        WHERE b.base_sha = t.landed_sha
                          AND a2.step = 'code'
                          AND a2.attempt_no = (SELECT MIN(a3.attempt_no) FROM attempts a3 WHERE a3.task_id = a2.task_id AND a3.step = 'code')
                          AND EXISTS (
                              SELECT 1 FROM json_each(a2.verdict_json) j
                              WHERE json_extract(j.value, '$.level') = 'L1' AND json_extract(j.value, '$.ok') = 0
                          )
                    ) THEN t.id END)
             FROM attempts a JOIN tasks t ON t.id = a.task_id
             WHERE a.state != 'running'
             GROUP BY a.step, a.provider, attempt_model
             ORDER BY a.step, a.provider, attempt_model",
            )?;
            let rows = stmt.query_map([], |r| {
                let role: String = r.get(0)?;
                let landed: i64 = r.get(8)?;
                let broke_base: i64 = r.get(9)?;
                let is_code = role == "code";
                Ok(RoleStat {
                    role,
                    provider: r.get(1)?,
                    model: r.get(2)?,
                    attempts: r.get(3)?,
                    succeeded: r.get(4)?,
                    mean_turns: r.get(5)?,
                    mean_cost_usd: r.get(6)?,
                    mean_ms: r.get(7)?,
                    landed: is_code.then_some(landed),
                    broke_base: is_code.then_some(broke_base),
                    repair_cost: is_code.then_some(0.0),
                    added_lines: is_code.then_some(0),
                    churned_lines: is_code.then_some(0),
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<RoleStat>>>()?
        };
        let landed = self.landed_tasks(&StatsFilter::default())?;
        let c = self.lock();
        for t in &landed {
            let groups = code_attempt_groups_query(&c, t.id)?;
            if groups.is_empty() {
                continue;
            }
            let cost = repair_cost_cache_query(&c, t.id)?.map(|(cost, _)| cost);
            let churn = churn_cache_query(&c, t.id)?;
            for (provider, model) in groups {
                let Some(r) = stats
                    .iter_mut()
                    .find(|r| r.role == "code" && r.provider == provider && r.model == model)
                else {
                    continue;
                };
                if let Some(cost) = cost {
                    r.repair_cost = Some(r.repair_cost.unwrap_or(0.0) + cost);
                }
                if let Some((added, churned, _)) = churn {
                    r.added_lines = Some(r.added_lines.unwrap_or(0) + added);
                    r.churned_lines = Some(r.churned_lines.unwrap_or(0) + churned);
                }
            }
        }
        Ok(stats)
    }

    /// The listing behind `forge log`: newest first, filtered, and paged by
    /// `before` (ids strictly below it) so a client can scroll back.
    pub fn list_tasks_where(&self, q: &TaskFilter) -> Result<Vec<TaskSummary>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.id, t.state, datetime(t.created_at,'unixepoch','localtime'), t.repo, t.task,
                    (SELECT COUNT(*) FROM attempts a WHERE a.task_id=t.id),
                    (SELECT COALESCE(SUM(cost_usd),0) FROM attempts a WHERE a.task_id=t.id),
                    t.workflow, t.created_at, t.finished_at, t.project, t.initiative
             FROM tasks t WHERE (?2 IS NULL OR t.state = ?2) AND (?3 IS NULL OR t.repo = ?3)
               AND (?4 IS NULL OR t.id < ?4)
               AND (?5 IS NULL OR t.task LIKE '%' || ?5 || '%' OR CAST(t.id AS TEXT) = ?5)
               AND (?6 IS NULL OR t.workflow = ?6)
               AND (?7 IS NULL OR t.project = ?7)
               AND (?8 IS NULL OR t.initiative = ?8)
             ORDER BY t.id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(
            params![
                q.limit,
                q.state.map(TaskState::as_str),
                q.repo.as_deref(),
                q.before,
                q.grep.as_deref(),
                q.workflow.as_deref(),
                q.project.as_deref(),
                q.initiative,
            ],
            |r| {
                Ok(TaskSummary {
                    id: r.get(0)?,
                    state: r.get(1)?,
                    created: r.get(2)?,
                    repo: r.get(3)?,
                    task: r.get(4)?,
                    attempts: r.get(5)?,
                    cost: r.get(6)?,
                    workflow: r.get(7)?,
                    created_at: r.get(8)?,
                    finished_at: r.get(9)?,
                    project: r.get(10)?,
                    initiative: r.get(11)?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Record an answer to a blocked task's question. `answered_for` is
    /// who the question was addressed to (the task's `question_to` at
    /// answer time), copied here since the task it retries into carries
    /// no such field forward.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_decision_by(
        &self,
        task_id: i64,
        repo: &str,
        question: &str,
        answer: &str,
        answered_by: &str,
        citations: &str,
        answered_for: Option<&str>,
    ) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO decisions (task_id, repo, question, answer, created_at, answered_by, citations, answered_for) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![task_id, repo, question, answer, crate::unix_now(), answered_by, citations, answered_for],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn set_decision_retry(&self, decision_id: i64, retry_id: i64) -> Result<()> {
        self.lock().execute(
            "UPDATE decisions SET retry_id=?2 WHERE id=?1",
            params![decision_id, retry_id],
        )?;
        Ok(())
    }

    /// How many times the supervisor has answered within this piece of work.
    pub fn supervisor_answers_in_lineage(&self, task_id: i64) -> Result<u32> {
        // self.lineage() walks *down* the whole retry tree from the root, so
        // it counts every branch, not just task_id's own ancestor chain; a
        // task can share a retry_of with a sibling (see dependents_retries /
        // latest_retry_of), so this must not narrow to task_id's own chain.
        // Computed before locking below: lineage() takes the lock itself.
        let ids: Vec<i64> = self.lineage(task_id)?.iter().map(|l| l.id).collect();
        let c = self.lock();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let n: i64 = c.query_row(
            &format!(
                "SELECT COUNT(*) FROM decisions WHERE task_id IN ({placeholders}) AND answered_by='supervisor'"
            ),
            rusqlite::params_from_iter(ids.iter()),
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    /// Every decision recorded on `id` or any task it retries, oldest first.
    pub fn decisions_in_lineage(&self, id: i64) -> Result<Vec<Decision>> {
        let c = self.lock();
        let ids = lineage_ids(&c, id)?;
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut stmt = c.prepare(&format!(
            "SELECT d.id, d.task_id, d.repo, d.question, d.answer, d.created_at, d.answered_by, d.citations, d.retry_id, d.answered_for
             FROM decisions d WHERE d.task_id IN ({placeholders})
             ORDER BY d.id"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
            Ok(Decision {
                id: r.get(0)?,
                task_id: r.get(1)?,
                repo: r.get(2)?,
                question: r.get(3)?,
                answer: r.get(4)?,
                created_at: r.get(5)?,
                answered_by: r.get(6)?,
                citations: r.get(7)?,
                retry_id: r.get(8)?,
                answered_for: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every plugin recorded as enabled.
    pub fn enabled_plugins(&self) -> Result<std::collections::BTreeSet<String>> {
        let c = self.lock();
        let mut stmt = c.prepare("SELECT name FROM plugins WHERE enabled = 1")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Set a plugin's enabled flag, recording when it was enabled (`None`
    /// when disabling).
    pub fn set_plugin_enabled(&self, name: &str, enabled: bool, at: i64) -> Result<()> {
        let c = self.lock();
        c.execute(
            "INSERT INTO plugins (name, enabled, enabled_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET enabled = excluded.enabled, enabled_at = excluded.enabled_at",
            params![name, enabled as i64, enabled.then_some(at)],
        )?;
        Ok(())
    }

    /// Recorded answers, newest first; narrowed by repository, project,
    /// and/or initiative when given. Project and initiative narrow
    /// through the task the decision was recorded on, since a decision
    /// carries no such column of its own.
    pub fn decisions(&self, q: &DecisionFilter) -> Result<Vec<Decision>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT d.id, d.task_id, d.repo, d.question, d.answer, d.created_at, d.answered_by, d.citations, d.retry_id, d.answered_for
             FROM decisions d JOIN tasks t ON t.id = d.task_id
             WHERE (?1 IS NULL OR d.repo = ?1)
               AND (?2 IS NULL OR t.project = ?2)
               AND (?3 IS NULL OR t.initiative = ?3)
             ORDER BY d.id DESC",
        )?;
        let rows = stmt.query_map(params![q.repo, q.project, q.initiative], |r| {
            Ok(Decision {
                id: r.get(0)?,
                task_id: r.get(1)?,
                repo: r.get(2)?,
                question: r.get(3)?,
                answer: r.get(4)?,
                created_at: r.get(5)?,
                answered_by: r.get(6)?,
                citations: r.get(7)?,
                retry_id: r.get(8)?,
                answered_for: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Record a reference on a task: the pull request it landed as, the
    /// issue it came from.
    pub fn insert_task_ref(
        &self,
        task_id: i64,
        kind: &str,
        url: &str,
        label: &str,
        by: &str,
    ) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO task_refs (task_id, kind, url, label, by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![task_id, kind, url, label, by, crate::unix_now()],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// A task's references, oldest first.
    pub fn task_refs(&self, task_id: i64) -> Result<Vec<TaskRef>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT id, task_id, kind, url, label, by, created_at FROM task_refs WHERE task_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![task_id], |r| {
            Ok(TaskRef {
                id: r.get(0)?,
                task_id: r.get(1)?,
                kind: r.get(2)?,
                url: r.get(3)?,
                label: r.get(4)?,
                by: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Register a new project. Fails if the name is already taken.
    pub fn create_project(&self, p: &Project) -> Result<()> {
        self.lock().execute(
            "INSERT INTO projects (name, purpose, created_at) VALUES (?1, ?2, ?3)",
            params![p.name, p.purpose, p.created_at],
        )?;
        Ok(())
    }

    pub fn project(&self, name: &str) -> Result<Option<Project>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT name, purpose, created_at, workflow, per_task_usd, per_initiative_usd,
                        supervisor_model, supervisor_per_lineage, protected_json, role_providers_json
                 FROM projects WHERE name=?1",
                params![name],
                project_from_row,
            )
            .optional()?)
    }

    /// Every project, alphabetically.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT name, purpose, created_at, workflow, per_task_usd, per_initiative_usd,
                    supervisor_model, supervisor_per_lineage, protected_json, role_providers_json
             FROM projects ORDER BY name",
        )?;
        let rows = stmt.query_map([], project_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Apply `forge project set`'s changes: only the columns given (not
    /// `None`) change; `role_providers` merges into the existing map
    /// instead of replacing it, so setting one role leaves the others
    /// alone. Returns `false` if no project has this name.
    pub fn set_project_defaults(&self, name: &str, d: &ProjectDefaults) -> Result<bool> {
        let protected = d
            .protected
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let role_providers = if d.role_providers.is_empty() {
            None
        } else {
            let current: Option<String> = self
                .lock()
                .query_row(
                    "SELECT role_providers_json FROM projects WHERE name=?1",
                    params![name],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let mut merged: BTreeMap<String, String> = current
                .as_deref()
                .map(|j| serde_json::from_str(j).unwrap_or_default())
                .unwrap_or_default();
            merged.extend(d.role_providers.clone());
            Some(serde_json::to_string(&merged)?)
        };
        let n = self.lock().execute(
            "UPDATE projects SET
                workflow = COALESCE(?2, workflow),
                per_task_usd = COALESCE(?3, per_task_usd),
                per_initiative_usd = COALESCE(?4, per_initiative_usd),
                supervisor_model = COALESCE(?5, supervisor_model),
                supervisor_per_lineage = COALESCE(?6, supervisor_per_lineage),
                protected_json = COALESCE(?7, protected_json),
                role_providers_json = COALESCE(?8, role_providers_json)
             WHERE name=?1",
            params![
                name,
                d.workflow,
                d.per_task_usd,
                d.per_initiative_usd,
                d.supervisor_model,
                d.supervisor_per_lineage,
                protected,
                role_providers,
            ],
        )?;
        Ok(n > 0)
    }

    /// Every project that lists `repo`, alphabetically: the names an
    /// ambiguous-repository refusal names.
    pub fn projects_listing_repo(&self, repo: &str) -> Result<Vec<String>> {
        let c = self.lock();
        let mut stmt =
            c.prepare("SELECT project FROM project_repos WHERE repo=?1 ORDER BY project")?;
        let rows = stmt.query_map(params![repo], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Add a backlog item to a project. Returns its id.
    pub fn add_backlog(&self, project: &str, text: &str) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO backlog (project, text, created_at) VALUES (?1, ?2, ?3)",
            params![project, text, crate::unix_now()],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// A project's backlog, oldest first.
    pub fn backlog(&self, project: &str) -> Result<Vec<BacklogItem>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT id, project, text, created_at, done_at FROM backlog WHERE project=?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![project], |r| {
            Ok(BacklogItem {
                id: r.get(0)?,
                project: r.get(1)?,
                text: r.get(2)?,
                created_at: r.get(3)?,
                done_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Mark a backlog item done. `false` if it does not exist in this
    /// project or is already done.
    pub fn mark_backlog_done(&self, project: &str, id: i64) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE backlog SET done_at=?3 WHERE id=?1 AND project=?2 AND done_at IS NULL",
            params![id, project, crate::unix_now()],
        )?;
        Ok(n > 0)
    }

    /// List a repository under a project, with an optional scope (the
    /// paths within it the project owns; `None` means the whole
    /// repository). Registering the same pair again replaces the scope.
    pub fn register_repo(&self, project: &str, repo: &str, scope: Option<&str>) -> Result<()> {
        self.lock().execute(
            "INSERT INTO project_repos (project, repo, scope_json) VALUES (?1, ?2, ?3)
             ON CONFLICT(project, repo) DO UPDATE SET scope_json = excluded.scope_json",
            params![project, repo, scope],
        )?;
        Ok(())
    }

    /// The project's first repository, in the order it was registered
    /// (`forge project new --repo` lists it first, or a lone `forge
    /// project new ... --repo` call the only one): what an initiative's
    /// `--from` file falls to for a paragraph with no `repo:` line.
    pub fn first_repo(&self, project: &str) -> Result<Option<String>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT repo FROM project_repos WHERE project=?1 ORDER BY rowid LIMIT 1",
                params![project],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// A project's repositories, alphabetically.
    pub fn project_repos(&self, project: &str) -> Result<Vec<ProjectRepo>> {
        let c = self.lock();
        let mut stmt =
            c.prepare("SELECT repo, scope_json FROM project_repos WHERE project=?1 ORDER BY repo")?;
        let rows = stmt.query_map(params![project], |r| {
            Ok(ProjectRepo {
                repo: r.get(0)?,
                scope: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The project that lists `repo`, when exactly one does; `None` if no
    /// project lists it, or more than one does.
    pub fn default_project_for_repo(&self, repo: &str) -> Result<Option<String>> {
        let c = self.lock();
        let mut stmt = c.prepare("SELECT project FROM project_repos WHERE repo=?1")?;
        let names: Vec<String> = stmt
            .query_map(params![repo], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(match names.len() {
            1 => names.into_iter().next(),
            _ => None,
        })
    }

    /// The default project for `repo`, creating one if the repository is
    /// not yet listed by any project: named by the repository's base
    /// name, or `forge` for the Forge repository itself, the same rule
    /// the migration applies to pre-existing tasks. `None` when the
    /// repository is already listed by more than one project (ambiguous;
    /// `queue::enqueue` turns that into a refusal naming them, since only
    /// the operator can say which with `forge add --project`).
    pub fn ensure_default_project(&self, repo: &str) -> Result<Option<String>> {
        if let Some(name) = self.default_project_for_repo(repo)? {
            return Ok(Some(name));
        }
        let ambiguous: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM project_repos WHERE repo=?1",
            params![repo],
            |r| r.get(0),
        )?;
        if ambiguous > 0 {
            return Ok(None);
        }
        let name = project_name_for_repo(repo);
        if self.project(&name)?.is_none() {
            self.create_project(&Project {
                name: name.clone(),
                purpose: format!("Repository {repo}."),
                created_at: crate::unix_now(),
                ..Default::default()
            })?;
        }
        self.register_repo(&name, repo, None)?;
        Ok(Some(name))
    }

    /// Register a new initiative. Returns its id.
    pub fn create_initiative(&self, ini: &Initiative) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO initiatives (project, outcome, budget_usd, stop_after_same_rule, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                ini.project,
                ini.outcome,
                ini.budget_usd,
                ini.stop_after_same_rule,
                ini.created_at
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Change only the fields `d` gives; returns `false` if `id` names no
    /// initiative.
    pub fn set_initiative(&self, id: i64, d: &InitiativeUpdate) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE initiatives SET
                outcome = COALESCE(?2, outcome),
                budget_usd = COALESCE(?3, budget_usd),
                stop_after_same_rule = COALESCE(?4, stop_after_same_rule)
             WHERE id=?1",
            params![id, d.outcome, d.budget_usd, d.stop_after_same_rule],
        )?;
        Ok(n > 0)
    }

    pub fn initiative(&self, id: i64) -> Result<Option<Initiative>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT id, project, outcome, budget_usd, stop_after_same_rule, created_at, settled_at
                 FROM initiatives WHERE id=?1",
                params![id],
                initiative_from_row,
            )
            .optional()?)
    }

    /// Every initiative, oldest first; only `project`'s when given.
    pub fn list_initiatives(&self, project: Option<&str>) -> Result<Vec<Initiative>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT id, project, outcome, budget_usd, stop_after_same_rule, created_at, settled_at
             FROM initiatives WHERE ?1 IS NULL OR project = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![project], initiative_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// An initiative's tasks, oldest first.
    pub fn initiative_tasks(&self, id: i64) -> Result<Vec<Task>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM tasks WHERE initiative=?1 ORDER BY id",
            TASK_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![id], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// A project's tasks, oldest first.
    pub fn project_tasks(&self, project: &str) -> Result<Vec<Task>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM tasks WHERE project=?1 ORDER BY id",
            TASK_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![project], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The summed cost of every attempt of every one of an initiative's tasks.
    pub fn initiative_cost(&self, id: i64) -> Result<f64> {
        Ok(self.lock().query_row(
            "SELECT COALESCE(SUM(a.cost_usd), 0) FROM attempts a
             WHERE a.task_id IN (SELECT id FROM tasks WHERE initiative=?1)",
            params![id],
            |r| r.get(0),
        )?)
    }

    /// Every initiative id with at least one queued task: the only ones
    /// worth checking for a hold before the worker claims (see
    /// `view::initiative_hold`).
    pub fn initiatives_with_queued_tasks(&self) -> Result<Vec<i64>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT DISTINCT initiative FROM tasks WHERE state='queued' AND initiative IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Mark an initiative settled now. `false` if it already was.
    pub fn settle_initiative(&self, id: i64, at: i64) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE initiatives SET settled_at=?2 WHERE id=?1 AND settled_at IS NULL",
            params![id, at],
        )?;
        Ok(n > 0)
    }

    /// Task counts by state and total cost for one project.
    pub fn project_task_stats(&self, project: &str) -> Result<ProjectTaskStats> {
        let c = self.lock();
        Ok(c.query_row(
            "SELECT SUM(state='queued'), SUM(state='running'), SUM(state='succeeded'), SUM(state='failed'),
                    SUM(state='unverified'), SUM(state='blocked'), SUM(state='withdrawn'),
                    COALESCE((SELECT SUM(a.cost_usd) FROM attempts a WHERE a.task_id IN
                        (SELECT id FROM tasks WHERE project=?1)), 0)
             FROM tasks WHERE project=?1",
            params![project],
            |r| {
                Ok(ProjectTaskStats {
                    queued: r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    running: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    succeeded: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    failed: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    unverified: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    blocked: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    withdrawn: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    cost: r.get(7)?,
                })
            },
        )?)
    }

    /// Tasks, landed count, cost and defect escape per project: what
    /// `forge stats` adds below the per-workflow table when it is not
    /// itself scoped to one project or initiative (see docs/PROJECTS.md,
    /// "The record, scoped").
    pub fn project_stats(&self) -> Result<Vec<ProjectStat>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.project, COUNT(*), SUM(t.landed_sha != ''),
                    COALESCE((SELECT SUM(a.cost_usd) FROM attempts a WHERE a.task_id IN (SELECT id FROM tasks t2 WHERE t2.project=t.project)), 0),
                    SUM(t.landed_sha != '' AND EXISTS (
                        SELECT 1 FROM attempts a
                        JOIN tasks b ON b.id = a.task_id
                        WHERE b.base_sha = t.landed_sha
                          AND a.step = 'code'
                          AND a.attempt_no = (SELECT MIN(a2.attempt_no) FROM attempts a2 WHERE a2.task_id = a.task_id AND a2.step = 'code')
                          AND EXISTS (
                              SELECT 1 FROM json_each(a.verdict_json) j
                              WHERE json_extract(j.value, '$.level') = 'L1' AND json_extract(j.value, '$.ok') = 0
                          )
                    ))
             FROM tasks t WHERE t.project IS NOT NULL AND t.state IN ('succeeded','failed','blocked','unverified') AND t.started_at IS NOT NULL
             GROUP BY t.project ORDER BY t.project",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ProjectStat {
                project: r.get(0)?,
                tasks: r.get(1)?,
                landed: r.get(2)?,
                cost: r.get(3)?,
                broke_base: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Declare a deploy target. Fails if `(project, name)` already exists.
    pub fn add_deploy_target(&self, t: &DeployTarget) -> Result<()> {
        let args_json = serde_json::to_string(&t.args)?;
        self.lock().execute(
            "INSERT INTO deploy_targets (project, name, repo, scope_json, method, args_json, check_cmd, on_landing, smoke_url)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                t.project,
                t.name,
                t.repo,
                t.scope,
                t.method,
                args_json,
                t.check_cmd,
                t.on_landing,
                t.smoke_url,
            ],
        )?;
        Ok(())
    }

    /// Replace a deploy target's fields in place (project and name stay
    /// the primary key): what `forge project deploy set` writes after
    /// merging only the flags given onto the row `deploy_target` returned.
    pub fn update_deploy_target(&self, t: &DeployTarget) -> Result<()> {
        let args_json = serde_json::to_string(&t.args)?;
        let n = self.lock().execute(
            "UPDATE deploy_targets SET repo=?3, scope_json=?4, method=?5, args_json=?6, check_cmd=?7, on_landing=?8, smoke_url=?9
             WHERE project=?1 AND name=?2",
            params![
                t.project,
                t.name,
                t.repo,
                t.scope,
                t.method,
                args_json,
                t.check_cmd,
                t.on_landing,
                t.smoke_url,
            ],
        )?;
        if n != 1 {
            bail!("no deploy target {} in project {}", t.name, t.project);
        }
        Ok(())
    }

    /// Delete a deploy target. Its past deploys (`deploys`, `forge deploy
    /// log`) are untouched; only future `--on-landing` runs and `forge
    /// deploy` of this name stop.
    pub fn remove_deploy_target(&self, project: &str, name: &str) -> Result<()> {
        let n = self.lock().execute(
            "DELETE FROM deploy_targets WHERE project=?1 AND name=?2",
            params![project, name],
        )?;
        if n != 1 {
            bail!("no deploy target {name} in project {project}");
        }
        Ok(())
    }

    /// One project's deploy target by name.
    pub fn deploy_target(&self, project: &str, name: &str) -> Result<Option<DeployTarget>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT project, name, repo, scope_json, method, args_json, check_cmd, on_landing, smoke_url
                 FROM deploy_targets WHERE project=?1 AND name=?2",
                params![project, name],
                deploy_target_from_row,
            )
            .optional()?)
    }

    /// A project's deploy targets, alphabetically.
    pub fn deploy_targets(&self, project: &str) -> Result<Vec<DeployTarget>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT project, name, repo, scope_json, method, args_json, check_cmd, on_landing, smoke_url
             FROM deploy_targets WHERE project=?1 ORDER BY name",
        )?;
        let rows = stmt.query_map(params![project], deploy_target_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Record a deploy starting, optionally tied to the task that landed
    /// and triggered it. Returns its id; `finish_deploy` completes it.
    pub fn start_deploy(
        &self,
        project: &str,
        target: &str,
        sha: &str,
        at: i64,
        task_id: Option<i64>,
    ) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO deploys (project, target, sha, started_at, task_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![project, target, sha, at, task_id],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Record a deploy's outcome: the check's verdict and output, what it
    /// rolled back to (if it did), why, and the smoke operation's verdict
    /// when the target declared a smoke url and the check passed for it to
    /// run (`None`, `None` otherwise).
    #[allow(clippy::too_many_arguments)]
    pub fn finish_deploy(
        &self,
        id: i64,
        at: i64,
        check_ok: bool,
        check_output: &str,
        rolled_back_to: Option<&str>,
        reason: &str,
        smoke_ok: Option<bool>,
        smoke_json: Option<&str>,
    ) -> Result<()> {
        self.lock().execute(
            "UPDATE deploys SET finished_at=?2, check_ok=?3, check_output=?4, rolled_back_to=?5, reason=?6, smoke_ok=?7, smoke_json=?8
             WHERE id=?1",
            params![
                id,
                at,
                check_ok,
                check_output,
                rolled_back_to,
                reason,
                smoke_ok,
                smoke_json
            ],
        )?;
        Ok(())
    }

    /// A project's deploys, newest first; only `target`'s when given: what
    /// `forge deploy log` shows.
    pub fn deploys(&self, project: &str, target: Option<&str>) -> Result<Vec<Deploy>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT id, project, target, sha, started_at, finished_at, check_ok, check_output, rolled_back_to, reason, task_id, smoke_ok, smoke_json
             FROM deploys WHERE project=?1 AND (?2 IS NULL OR target=?2) ORDER BY id DESC",
        )?;
        let rows = stmt.query_map(params![project, target], deploy_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// A task's deploys, newest first: the on-landing targets it triggered
    /// when it landed (see docs/DEPLOY.md, "When a deploy runs"). What
    /// `forge show`, `forge trace --json`, and the web task view list
    /// under the task.
    pub fn deploys_for_task(&self, task_id: i64) -> Result<Vec<Deploy>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT id, project, target, sha, started_at, finished_at, check_ok, check_output, rolled_back_to, reason, task_id, smoke_ok, smoke_json
             FROM deploys WHERE task_id=?1 ORDER BY id DESC",
        )?;
        let rows = stmt.query_map(params![task_id], deploy_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Record one assess directive run against a landed task. Returns its id.
    pub fn insert_assessment(&self, a: &Assessment) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO assessments (task_id, score, findings_json, model, provider, cost_usd, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                a.task_id,
                a.score,
                a.findings_json,
                a.model,
                a.provider,
                a.cost_usd,
                a.created_at
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// A task's most recent assessment, if the assess directive has ever
    /// run against it (see docs/ACTIONS.md, "Assessment").
    pub fn assessment(&self, task_id: i64) -> Result<Option<Assessment>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT id, task_id, score, findings_json, model, provider, cost_usd, created_at
                 FROM assessments WHERE task_id=?1 ORDER BY id DESC LIMIT 1",
                params![task_id],
                |r| {
                    Ok(Assessment {
                        id: r.get(0)?,
                        task_id: r.get(1)?,
                        score: r.get(2)?,
                        findings_json: r.get(3)?,
                        model: r.get(4)?,
                        provider: r.get(5)?,
                        cost_usd: r.get(6)?,
                        created_at: r.get(7)?,
                    })
                },
            )
            .optional()?)
    }
}

fn deploy_target_from_row(r: &Row) -> rusqlite::Result<DeployTarget> {
    let args_json: String = r.get(5)?;
    Ok(DeployTarget {
        project: r.get(0)?,
        name: r.get(1)?,
        repo: r.get(2)?,
        scope: r.get(3)?,
        method: r.get(4)?,
        args: serde_json::from_str(&args_json).unwrap_or_default(),
        check_cmd: r.get(6)?,
        on_landing: r.get(7)?,
        smoke_url: r.get(8)?,
    })
}

fn deploy_from_row(r: &Row) -> rusqlite::Result<Deploy> {
    Ok(Deploy {
        id: r.get(0)?,
        project: r.get(1)?,
        target: r.get(2)?,
        sha: r.get(3)?,
        started_at: r.get(4)?,
        finished_at: r.get(5)?,
        check_ok: r.get(6)?,
        check_output: r.get(7)?,
        rolled_back_to: r.get(8)?,
        reason: r.get(9)?,
        task_id: r.get(10)?,
        smoke_ok: r.get(11)?,
        smoke_json: r.get(12)?,
    })
}

fn project_from_row(r: &Row) -> rusqlite::Result<Project> {
    let protected_json: Option<String> = r.get(8)?;
    let role_providers_json: Option<String> = r.get(9)?;
    Ok(Project {
        name: r.get(0)?,
        purpose: r.get(1)?,
        created_at: r.get(2)?,
        workflow: r.get(3)?,
        per_task_usd: r.get(4)?,
        per_initiative_usd: r.get(5)?,
        supervisor_model: r.get(6)?,
        supervisor_per_lineage: r.get(7)?,
        protected: protected_json.map(|j| serde_json::from_str(&j).unwrap_or_default()),
        role_providers: role_providers_json
            .map(|j| serde_json::from_str(&j).unwrap_or_default())
            .unwrap_or_default(),
    })
}

fn initiative_from_row(r: &Row) -> rusqlite::Result<Initiative> {
    Ok(Initiative {
        id: r.get(0)?,
        project: r.get(1)?,
        outcome: r.get(2)?,
        budget_usd: r.get(3)?,
        stop_after_same_rule: r.get(4)?,
        created_at: r.get(5)?,
        settled_at: r.get(6)?,
    })
}

#[derive(serde::Deserialize)]
struct CargoManifest {
    package: Option<CargoPackage>,
}

#[derive(serde::Deserialize)]
struct CargoPackage {
    name: String,
}

/// Whether `repo` is the Forge repository itself: its `Cargo.toml`
/// declares the same package name this very binary was built from,
/// regardless of what the checkout directory happens to be called (a
/// worktree, a fork, a differently-named clone).
fn is_forge_repo(repo: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(Path::new(repo).join("Cargo.toml")) else {
        return false;
    };
    let Ok(manifest) = toml::from_str::<CargoManifest>(&text) else {
        return false;
    };
    manifest.package.map(|p| p.name).as_deref() == Some(env!("CARGO_PKG_NAME"))
}

/// The project name a repository gets from the migration and from
/// `Store::ensure_default_project`: the repository's base name, except
/// the Forge repository itself, which is always named `forge`.
fn project_name_for_repo(repo: &str) -> String {
    if is_forge_repo(repo) {
        return "forge".to_string();
    }
    Path::new(repo)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(repo)
        .to_string()
}

/// Create the project `repo` would get from the migration, if none
/// exists yet, and register the repository under it with no scope.
/// Idempotent: a repository already listed keeps its existing project.
fn seed_project_for_repo(conn: &Connection, repo: &str) -> rusqlite::Result<String> {
    let name = project_name_for_repo(repo);
    conn.execute(
        "INSERT INTO projects (name, purpose, created_at) VALUES (?1, ?2, ?3) ON CONFLICT(name) DO NOTHING",
        params![name, format!("Repository {repo}."), crate::unix_now()],
    )?;
    conn.execute(
        "INSERT INTO project_repos (project, repo, scope_json) VALUES (?1, ?2, NULL)
         ON CONFLICT(project, repo) DO NOTHING",
        params![name, repo],
    )?;
    Ok(name)
}

/// The migration's data half: every distinct repository already in
/// `tasks` gets a project (see `seed_project_for_repo`), and every task
/// in that repository is assigned to it. Run once, when `migrate` applies
/// `PROJECTS_MIGRATION_VERSION`.
fn seed_projects_from_tasks(conn: &Connection) -> rusqlite::Result<()> {
    let repos: Vec<String> = {
        let mut stmt = conn.prepare("SELECT DISTINCT repo FROM tasks")?;
        stmt.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for repo in repos {
        let name = seed_project_for_repo(conn, &repo)?;
        conn.execute(
            "UPDATE tasks SET project = ?1 WHERE repo = ?2",
            params![name, repo],
        )?;
    }
    Ok(())
}

fn migrate(conn: &Connection) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let target = MIGRATIONS.len() as i64;
    if current > target {
        bail!(
            "database schema version {current} is newer than this forge ({target}); upgrade forge"
        );
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let v = i as i64 + 1;
        conn.execute_batch("BEGIN")?;
        let r: rusqlite::Result<()> = (|| {
            conn.execute_batch(sql)?;
            if v == PROJECTS_MIGRATION_VERSION {
                seed_projects_from_tasks(conn)?;
            }
            conn.execute_batch(&format!("PRAGMA user_version={v}"))
        })();
        match r {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(e) => {
                conn.execute_batch("ROLLBACK").ok();
                bail!("migration to schema version {v} failed: {e}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_fresh_db_to_latest_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len() as i64);
        drop(s);
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len() as i64);
    }

    #[test]
    fn refuses_a_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch("PRAGMA user_version=999").unwrap();
        }
        let err = match Store::open(&path) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("opened a db from the future"),
        };
        assert!(err.contains("newer than this forge"), "{err}");
    }

    #[test]
    fn migration_assigns_one_project_per_distinct_repo_naming_forge_specially() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");

        // A repository whose Cargo.toml declares it as the Forge package
        // itself, under a checkout directory that is not called "forge" —
        // the case the base-name rule alone would get wrong.
        let forge_dir = dir.path().join("some-worktree");
        std::fs::create_dir_all(&forge_dir).unwrap();
        std::fs::write(
            forge_dir.join("Cargo.toml"),
            "[package]\nname = \"forge\"\n",
        )
        .unwrap();
        let forge_dir = forge_dir.canonicalize().unwrap().display().to_string();

        let other_dir = dir.path().join("nucleosynthesis");
        std::fs::create_dir_all(&other_dir).unwrap();
        let other_dir = other_dir.canonicalize().unwrap().display().to_string();

        // Build a pre-projects fixture by hand: every migration up to but
        // not including this one, with two tasks already in the table.
        {
            let c = Connection::open(&path).unwrap();
            for sql in &MIGRATIONS[..(PROJECTS_MIGRATION_VERSION as usize - 1)] {
                c.execute_batch(sql).unwrap();
            }
            c.execute_batch(&format!(
                "PRAGMA user_version={}",
                PROJECTS_MIGRATION_VERSION - 1
            ))
            .unwrap();
            c.execute(
                "INSERT INTO tasks (repo, task, base_branch, model, max_turns, max_attempts, timeout_secs, state, created_at)
                 VALUES (?1, 'do a', 'main', 'sonnet', 10, 1, 60, 'succeeded', 1)",
                params![forge_dir],
            )
            .unwrap();
            c.execute(
                "INSERT INTO tasks (repo, task, base_branch, model, max_turns, max_attempts, timeout_secs, state, created_at)
                 VALUES (?1, 'do b', 'main', 'sonnet', 10, 1, 60, 'succeeded', 2)",
                params![other_dir],
            )
            .unwrap();
        }

        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len() as i64);

        let mut names: Vec<String> = s
            .list_projects()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["forge".to_string(), "nucleosynthesis".to_string()]
        );

        assert_eq!(
            s.task(1).unwrap().unwrap().project.as_deref(),
            Some("forge")
        );
        assert_eq!(
            s.task(2).unwrap().unwrap().project.as_deref(),
            Some("nucleosynthesis")
        );

        let repos = s.project_repos("forge").unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].repo, forge_dir);
        assert!(repos[0].scope.is_none());

        // Every existing task is assigned; initiatives stay null.
        assert!(s.task(1).unwrap().unwrap().initiative.is_none());
    }

    #[test]
    fn migration_backfills_the_supervisors_model_where_it_inherited_the_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");

        // Build a pre-fix fixture by hand: every migration but this one,
        // with a task and three attempts already in the table, as they'd
        // have been written before `attempt::new_attempt` stopped
        // clobbering a supervisor attempt's own model with the task's.
        {
            let c = Connection::open(&path).unwrap();
            for sql in &MIGRATIONS[..(SUPERVISOR_MODEL_BACKFILL_MIGRATION_VERSION as usize - 1)] {
                c.execute_batch(sql).unwrap();
            }
            c.execute_batch(&format!(
                "PRAGMA user_version={}",
                SUPERVISOR_MODEL_BACKFILL_MIGRATION_VERSION - 1
            ))
            .unwrap();
            c.execute(
                "INSERT INTO tasks (repo, task, base_branch, model, max_turns, max_attempts, timeout_secs, state, created_at)
                 VALUES ('r', 't', 'main', 'qwen3-coder:30b', 10, 1, 60, 'blocked', 1)",
                [],
            )
            .unwrap();
            // The bug: a supervisor attempt on the "anthropic" default,
            // recorded with the task's model instead of its own.
            c.execute(
                "INSERT INTO attempts (task_id, attempt_no, state, started_at, step, provider, inputs_json)
                 VALUES (1, 1, 'needs_input', 1, 'supervisor', 'anthropic', '{\"model\":\"qwen3-coder:30b\"}')",
                [],
            )
            .unwrap();
            // A supervisor attempt on a non-anthropic provider: an
            // explicit choice, not the inherited-default bug; untouched.
            c.execute(
                "INSERT INTO attempts (task_id, attempt_no, state, started_at, step, provider, inputs_json)
                 VALUES (1, 2, 'needs_input', 1, 'supervisor', 'openai', '{\"model\":\"qwen3-coder:30b\"}')",
                [],
            )
            .unwrap();
            // An ordinary code attempt on anthropic: not a supervisor row,
            // untouched even though it shares the provider.
            c.execute(
                "INSERT INTO attempts (task_id, attempt_no, state, started_at, step, provider, inputs_json)
                 VALUES (1, 3, 'succeeded', 1, 'code', 'anthropic', '{\"model\":\"qwen3-coder:30b\"}')",
                [],
            )
            .unwrap();
        }

        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len() as i64);

        let model_of = |a: &Attempt| {
            serde_json::from_str::<serde_json::Value>(&a.inputs_json).unwrap()["model"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let attempts = s.attempts(1).unwrap();
        let fixed = attempts
            .iter()
            .find(|a| a.attempt_no == 1)
            .expect("the anthropic supervisor row");
        assert_eq!(
            model_of(fixed),
            "opus",
            "backfilled to the supervisor's own default model"
        );

        let other_provider = attempts
            .iter()
            .find(|a| a.attempt_no == 2)
            .expect("the openai supervisor row");
        assert_eq!(
            model_of(other_provider),
            "qwen3-coder:30b",
            "not anthropic, left alone"
        );

        let code = attempts
            .iter()
            .find(|a| a.attempt_no == 3)
            .expect("the code row");
        assert_eq!(
            model_of(code),
            "qwen3-coder:30b",
            "not a supervisor row, left alone"
        );
    }

    #[test]
    fn default_project_for_repo_is_none_unless_exactly_one_project_lists_it() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        assert_eq!(s.default_project_for_repo("/r").unwrap(), None);

        s.create_project(&Project {
            name: "a".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
        s.register_repo("a", "/r", None).unwrap();
        assert_eq!(
            s.default_project_for_repo("/r").unwrap(),
            Some("a".to_string())
        );

        s.create_project(&Project {
            name: "b".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
        s.register_repo("b", "/r", None).unwrap();
        assert_eq!(
            s.default_project_for_repo("/r").unwrap(),
            None,
            "listed by two projects now"
        );
    }

    #[test]
    fn ensure_default_project_creates_one_the_first_time_a_repo_is_seen() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let repo = dir.path().join("myrepo").display().to_string();
        assert_eq!(
            s.ensure_default_project(&repo).unwrap(),
            Some("myrepo".to_string())
        );
        // Idempotent: the same project is reused, not duplicated.
        assert_eq!(
            s.ensure_default_project(&repo).unwrap(),
            Some("myrepo".to_string())
        );
        assert_eq!(s.list_projects().unwrap().len(), 1);
    }

    #[test]
    fn set_project_defaults_role_providers_merges_instead_of_replacing() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        s.create_project(&Project {
            name: "p".into(),
            purpose: "purpose".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
        s.set_project_defaults(
            "p",
            &ProjectDefaults {
                role_providers: [("code".to_string(), "devhome".to_string())].into(),
                ..Default::default()
            },
        )
        .unwrap();
        s.set_project_defaults(
            "p",
            &ProjectDefaults {
                role_providers: [("review".to_string(), "openai".to_string())].into(),
                ..Default::default()
            },
        )
        .unwrap();
        let p = s.project("p").unwrap().unwrap();
        assert_eq!(p.role_providers["code"], "devhome", "not clobbered");
        assert_eq!(p.role_providers["review"], "openai");
        assert_eq!(p.role_providers.len(), 2);

        // Naming the same role again overwrites just that entry.
        s.set_project_defaults(
            "p",
            &ProjectDefaults {
                role_providers: [("code".to_string(), "openai".to_string())].into(),
                ..Default::default()
            },
        )
        .unwrap();
        let p = s.project("p").unwrap().unwrap();
        assert_eq!(p.role_providers["code"], "openai");
        assert_eq!(p.role_providers["review"], "openai");
    }

    #[test]
    fn latest_rate_limit_is_keyed_by_provider() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let t = Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 2,
            timeout_secs: 1,
            ..Default::default()
        };
        let id = s.insert_task(&t).unwrap();
        let finish = |aid: i64, five_hour: f64| FinishAttempt {
            id: aid,
            state: AttemptState::Succeeded,
            reason: String::new(),
            finished_at: Some(2),
            agent_exit: Some(0),
            timed_out: false,
            num_turns: 1,
            tool_calls: 1,
            cost_usd: Some(0.0),
            agent_ms: 0,
            commits: 0,
            files_changed: 0,
            dirty: false,
            verdict_json: "[]".into(),
            result_text: String::new(),
            envelope_json: String::new(),
            rl_five_hour: Some(five_hour),
            rl_seven_day: None,
            rl_five_hour_resets: Some(2_000_000_000),
            rl_seven_day_resets: None,
            end_sha: String::new(),
            outputs_json: String::new(),
            session_id: String::new(),
            first_edit: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            early_signals: "[]".into(),
            early_near: "[]".into(),
        };
        let anthropic_attempt = s
            .insert_attempt(&Attempt {
                task_id: id,
                attempt_no: 1,
                started_at: 1,
                provider: "anthropic".into(),
                ..Default::default()
            })
            .unwrap();
        s.finish_attempt(&finish(anthropic_attempt, 0.95)).unwrap();
        let devhome_attempt = s
            .insert_attempt(&Attempt {
                task_id: id,
                attempt_no: 2,
                started_at: 2,
                provider: "devhome".into(),
                ..Default::default()
            })
            .unwrap();
        s.finish_attempt(&finish(devhome_attempt, 0.1)).unwrap();
        assert_eq!(
            s.latest_rate_limit("anthropic").unwrap().unwrap().five_hour,
            Some(0.95)
        );
        assert_eq!(
            s.latest_rate_limit("devhome").unwrap().unwrap().five_hour,
            Some(0.1)
        );
        assert!(s.latest_rate_limit("openai").unwrap().is_none());
    }

    #[test]
    fn unknown_state_is_an_error_not_a_default() {
        assert!(TaskState::try_from("bogus").is_err());
        assert!(AttemptState::try_from("bogus").is_err());
    }

    #[test]
    fn claim_is_exclusive_and_requeue_closes_the_open_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let t = Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 2,
            timeout_secs: 1,
            ..Default::default()
        };
        let id = s.insert_task(&t).unwrap();
        assert!(s.claim(id, 1).unwrap());
        assert!(!s.claim(id, 2).unwrap(), "second claim must fail");
        let a = Attempt {
            task_id: id,
            attempt_no: 1,
            started_at: 0,
            ..Default::default()
        };
        s.insert_attempt(&a).unwrap();
        s.requeue(id, "worker died").unwrap();
        let t = s.task(id).unwrap().unwrap();
        assert_eq!(t.state, TaskState::Queued);
        let att = s.attempts(id).unwrap();
        assert_eq!(att[0].state, AttemptState::AgentFailed);
        assert_eq!(att[0].reason, "worker died");
        assert_eq!(
            s.claim_next(3, &[], |_| false).unwrap().map(|t| t.id),
            Some(id)
        );
    }

    #[test]
    fn supervisor_answers_in_lineage_counts_sibling_retries_too() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let t = Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 2,
            timeout_secs: 1,
            ..Default::default()
        };
        let root = s.insert_task(&t).unwrap();
        let mut retry = t.clone();
        retry.retry_of = Some(root);
        let a = s.insert_task(&retry).unwrap();
        let b = s.insert_task(&retry).unwrap();
        s.insert_decision_by(a, "r", "q", "a", "supervisor", "", None)
            .unwrap();
        assert_eq!(s.supervisor_answers_in_lineage(b).unwrap(), 1);
    }

    #[test]
    fn defect_escape_counts_broke_base_and_repaired_once_each() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();

        let base_task = |started_at: i64| Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Succeeded,
            created_at: started_at,
            started_at: Some(started_at),
            finished_at: Some(started_at + 1),
            workflow: "direct".into(),
            ..Default::default()
        };

        // A lands.
        let mut a = base_task(1);
        a.id = s.insert_task(&a).unwrap();
        a.landed_sha = "aaaaaaaa".into();
        s.update_task(&a).unwrap();

        // B starts from A's landed sha, and its first (and only) code
        // attempt is red on that base: an L1 row fails before B has done
        // anything of its own.
        let mut b = base_task(2);
        b.base_sha = "aaaaaaaa".into();
        b.id = s.insert_task(&b).unwrap();
        s.update_task(&b).unwrap();
        let b_attempt = Attempt {
            task_id: b.id,
            attempt_no: 1,
            step: "code".into(),
            started_at: 2,
            ..Default::default()
        };
        let b_attempt_id = s.insert_attempt(&b_attempt).unwrap();
        s.finish_attempt(&FinishAttempt {
            id: b_attempt_id,
            state: AttemptState::ChecksFailed,
            reason: "L1 failed: test".into(),
            finished_at: Some(3),
            agent_exit: Some(0),
            timed_out: false,
            num_turns: 1,
            tool_calls: 1,
            cost_usd: Some(0.0),
            agent_ms: 0,
            commits: 0,
            files_changed: 0,
            dirty: false,
            verdict_json: r#"[{"level":"L1","name":"test","ok":false,"exit":1,"ms":0,"timed_out":false,"tail":"","failing_tests":[]}]"#.into(),
            result_text: String::new(),
            envelope_json: String::new(),
            rl_five_hour: None,
            rl_seven_day: None,
            rl_five_hour_resets: None,
            rl_seven_day_resets: None,
            end_sha: String::new(),
            outputs_json: String::new(),
            session_id: String::new(),
            first_edit: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            early_signals: "[]".into(),
            early_near: "[]".into(),
        })
        .unwrap();

        // C carries a repairs reference to A.
        let mut c = base_task(4);
        c.id = s.insert_task(&c).unwrap();
        s.update_task(&c).unwrap();
        s.insert_task_ref(
            c.id,
            "repairs",
            &format!("forge://task/{}", a.id),
            "",
            "operator",
        )
        .unwrap();

        let stats = s.workflow_stats(&StatsFilter::default()).unwrap();
        assert_eq!(stats.len(), 1);
        let w = &stats[0];
        assert_eq!(w.tasks, 3);
        assert_eq!(w.landed, 1, "only A landed");
        assert_eq!(w.broke_base, 1, "A counts once for breaking B's base");
        assert_eq!(w.repaired, 1, "A counts once as repaired by C");
    }

    /// The git-level line-overlap attribution itself (half of a rewritten
    /// task's cost, none for an untouched one) is exercised on a real
    /// fixture repository in view.rs's `stats_tests`, next to the churn
    /// test it shares a fixture style with. This is the SQL half: once
    /// `task_repair_cost` is populated, `workflow_stats` sums it across
    /// every landed task in the workflow, the same way it already sums
    /// `task_churn`.
    #[test]
    fn workflow_stats_sums_the_repair_cost_cache_over_landed_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();

        let landed = |finished_at: i64, landed_sha: &str| Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Succeeded,
            created_at: finished_at,
            started_at: Some(finished_at),
            finished_at: Some(finished_at),
            workflow: "direct".into(),
            landed_sha: landed_sha.into(),
            ..Default::default()
        };
        let insert = |mut t: Task| {
            t.id = s.insert_task(&t).unwrap();
            s.update_task(&t).unwrap();
            t
        };

        let a = insert(landed(1000, "asha"));
        let b = insert(landed(2000, "bsha"));
        s.set_repair_cost_cache(a.id, 3.5, 9999).unwrap();
        s.set_repair_cost_cache(b.id, 1.5, 9999).unwrap();

        let stats = s.workflow_stats(&StatsFilter::default()).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].repair_cost, 5.0, "3.5 + 1.5, cached per task");
    }

    #[test]
    fn journal_control_stats_splits_code_retries_by_whether_the_journal_was_shown() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let t = Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 4,
            timeout_secs: 1,
            ..Default::default()
        };
        let task_id = s.insert_task(&t).unwrap();

        let attempt = |attempt_no, step: &str, inputs_json: &str| Attempt {
            task_id,
            attempt_no,
            step: step.into(),
            started_at: 0,
            inputs_json: inputs_json.into(),
            ..Default::default()
        };
        let finish = |id, state, num_turns, first_edit, cost_usd| {
            s.finish_attempt(&FinishAttempt {
                id,
                state,
                reason: String::new(),
                finished_at: Some(1),
                agent_exit: Some(0),
                timed_out: false,
                num_turns,
                tool_calls: 1,
                cost_usd: Some(cost_usd),
                agent_ms: 0,
                commits: 1,
                files_changed: 1,
                dirty: false,
                verdict_json: "[]".into(),
                result_text: String::new(),
                envelope_json: String::new(),
                rl_five_hour: None,
                rl_seven_day: None,
                rl_five_hour_resets: None,
                rl_seven_day_resets: None,
                end_sha: String::new(),
                outputs_json: String::new(),
                session_id: String::new(),
                first_edit,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                early_signals: "[]".into(),
                early_near: "[]".into(),
            })
            .unwrap();
        };

        // Two retries handed a journal.
        let a = s
            .insert_attempt(&attempt(
                2,
                "code",
                r#"{"journal":"earlier attempt said..."}"#,
            ))
            .unwrap();
        finish(a, AttemptState::Succeeded, 30, Some(10), 1.0);
        let b = s
            .insert_attempt(&attempt(3, "code", r#"{"journal":"more history"}"#))
            .unwrap();
        finish(b, AttemptState::ChecksFailed, 20, Some(6), 0.5);

        // One retry with no journal (absent field).
        let c = s.insert_attempt(&attempt(2, "code", "{}")).unwrap();
        finish(c, AttemptState::Succeeded, 25, None, 0.6);

        // Excluded: a first attempt (never a retry) even though it carries
        // a journal, and a non-code step's retry.
        let d = s
            .insert_attempt(&attempt(
                1,
                "code",
                r#"{"journal":"ignored, first attempt"}"#,
            ))
            .unwrap();
        finish(d, AttemptState::Succeeded, 99, Some(1), 9.0);
        let e = s
            .insert_attempt(&attempt(
                2,
                "review",
                r#"{"journal":"ignored, wrong step"}"#,
            ))
            .unwrap();
        finish(e, AttemptState::Succeeded, 99, Some(1), 9.0);

        let stats = s.journal_control_stats().unwrap();
        assert_eq!(stats.len(), 2);
        let journal = stats.iter().find(|j| j.has_journal).expect("a journal row");
        assert_eq!(journal.attempts, 2);
        assert_eq!(journal.succeeded, 1);
        assert_eq!(journal.mean_turns, 25.0);
        assert_eq!(journal.mean_first_edit, Some(8.0));
        assert_eq!(journal.mean_cost_usd, 0.75);

        let no_journal = stats
            .iter()
            .find(|j| !j.has_journal)
            .expect("a no-journal row");
        assert_eq!(no_journal.attempts, 1);
        assert_eq!(no_journal.succeeded, 1);
        assert_eq!(no_journal.mean_turns, 25.0);
        assert_eq!(
            no_journal.mean_first_edit, None,
            "the only attempt never edited"
        );
        assert_eq!(no_journal.mean_cost_usd, 0.6);
    }

    #[test]
    fn role_stats_splits_by_provider_and_model_and_averages_within_each() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();

        let base_task = |started_at: i64| Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Succeeded,
            created_at: started_at,
            started_at: Some(started_at),
            finished_at: Some(started_at + 1),
            workflow: "direct".into(),
            ..Default::default()
        };
        let attempt = |task_id, step: &str, provider: &str, model: &str| Attempt {
            task_id,
            attempt_no: 1,
            step: step.into(),
            provider: provider.into(),
            started_at: 0,
            inputs_json: format!(r#"{{"model":"{model}"}}"#),
            ..Default::default()
        };
        let finish = |id, state, num_turns, cost_usd, agent_ms, verdict_json: &str| {
            s.finish_attempt(&FinishAttempt {
                id,
                state,
                reason: String::new(),
                finished_at: Some(1),
                agent_exit: Some(0),
                timed_out: false,
                num_turns,
                tool_calls: 1,
                cost_usd: Some(cost_usd),
                agent_ms,
                commits: 1,
                files_changed: 1,
                dirty: false,
                verdict_json: verdict_json.into(),
                result_text: String::new(),
                envelope_json: String::new(),
                rl_five_hour: None,
                rl_seven_day: None,
                rl_five_hour_resets: None,
                rl_seven_day_resets: None,
                end_sha: String::new(),
                outputs_json: String::new(),
                session_id: String::new(),
                first_edit: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                early_signals: "[]".into(),
                early_near: "[]".into(),
            })
            .unwrap();
        };

        // A: code / anthropic / sonnet, succeeds and lands.
        let mut a = base_task(1);
        a.id = s.insert_task(&a).unwrap();
        let a1 = s
            .insert_attempt(&attempt(a.id, "code", "anthropic", "sonnet"))
            .unwrap();
        finish(a1, AttemptState::Succeeded, 10, 1.0, 1000, "[]");
        a.landed_sha = "aaaaaaaa".into();
        s.update_task(&a).unwrap();

        // A also carries a review-step attempt: a different role, excluded
        // from landed/broke-base entirely.
        let a2 = s
            .insert_attempt(&attempt(a.id, "review", "anthropic", "sonnet"))
            .unwrap();
        finish(a2, AttemptState::Succeeded, 2, 0.1, 100, "[]");

        // B: same code / anthropic / sonnet group, fails, never lands.
        let mut b = base_task(2);
        b.id = s.insert_task(&b).unwrap();
        let b1 = s
            .insert_attempt(&attempt(b.id, "code", "anthropic", "sonnet"))
            .unwrap();
        finish(b1, AttemptState::ChecksFailed, 20, 3.0, 3000, "[]");
        s.update_task(&b).unwrap();

        // C: code / openai / gpt-5, its own group entirely, succeeds and lands.
        let mut c = base_task(3);
        c.id = s.insert_task(&c).unwrap();
        let c1 = s
            .insert_attempt(&attempt(c.id, "code", "openai", "gpt-5"))
            .unwrap();
        finish(c1, AttemptState::Succeeded, 5, 0.5, 500, "[]");
        c.landed_sha = "cccccccc".into();
        s.update_task(&c).unwrap();

        // D: code / anthropic / haiku, its own group; starts from A's landed
        // sha and is red on it, so A's group counts a broke-base.
        let mut d = base_task(4);
        d.base_sha = "aaaaaaaa".into();
        d.id = s.insert_task(&d).unwrap();
        let d1 = s
            .insert_attempt(&attempt(d.id, "code", "anthropic", "haiku"))
            .unwrap();
        finish(
            d1,
            AttemptState::ChecksFailed,
            1,
            0.0,
            0,
            r#"[{"level":"L1","name":"test","ok":false,"exit":1,"ms":0,"timed_out":false,"tail":"","failing_tests":[]}]"#,
        );
        s.update_task(&d).unwrap();

        let stats = s.role_stats().unwrap();
        assert_eq!(
            stats.len(),
            4,
            "code/anthropic/sonnet, code/openai/gpt-5, code/anthropic/haiku, review/anthropic/sonnet"
        );

        let find = |role: &str, provider: &str, model: &str| {
            stats
                .iter()
                .find(|r| r.role == role && r.provider == provider && r.model == model)
                .unwrap_or_else(|| panic!("no row for {role}/{provider}/{model}"))
        };

        let sonnet = find("code", "anthropic", "sonnet");
        assert_eq!(sonnet.attempts, 2);
        assert_eq!(sonnet.succeeded, 1);
        assert_eq!(sonnet.mean_turns, 15.0);
        assert_eq!(sonnet.mean_cost_usd, 2.0);
        assert_eq!(sonnet.mean_ms, 2000.0);
        assert_eq!(sonnet.landed, Some(1), "only A landed");
        assert_eq!(sonnet.broke_base, Some(1), "A broke D's base");

        let gpt = find("code", "openai", "gpt-5");
        assert_eq!(gpt.attempts, 1);
        assert_eq!(gpt.succeeded, 1);
        assert_eq!(gpt.mean_turns, 5.0);
        assert_eq!(gpt.mean_cost_usd, 0.5);
        assert_eq!(gpt.mean_ms, 500.0);
        assert_eq!(gpt.landed, Some(1));
        assert_eq!(gpt.broke_base, Some(0), "nothing based off C's landed sha");

        let haiku = find("code", "anthropic", "haiku");
        assert_eq!(haiku.attempts, 1);
        assert_eq!(haiku.succeeded, 0);
        assert_eq!(haiku.landed, Some(0), "D never landed");
        assert_eq!(haiku.broke_base, Some(0));

        let review = find("review", "anthropic", "sonnet");
        assert_eq!(review.attempts, 1);
        assert_eq!(review.succeeded, 1);
        assert_eq!(review.landed, None, "landed is code-only");
        assert_eq!(review.broke_base, None, "broke-base is code-only");
    }

    #[test]
    fn role_stats_counts_an_investigate_or_interview_question_as_a_success() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();

        let mut t = Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Blocked,
            created_at: 1,
            workflow: "direct".into(),
            ..Default::default()
        };
        t.id = s.insert_task(&t).unwrap();

        let attempt = |step: &str| Attempt {
            task_id: t.id,
            attempt_no: 1,
            step: step.into(),
            provider: "anthropic".into(),
            started_at: 0,
            inputs_json: r#"{"model":"m"}"#.into(),
            ..Default::default()
        };
        let question = |kind: &str| {
            format!(
                r#"{{"schema_version":1,"summary":"s","needs_input":{{"question":"q","tried":"t","kind":"{kind}"}},"changes":[],"checks_run":[],"claims":[]}}"#
            )
        };
        let finish = |id, state, envelope_json: &str| {
            s.finish_attempt(&FinishAttempt {
                id,
                state,
                reason: String::new(),
                finished_at: Some(1),
                agent_exit: Some(0),
                timed_out: false,
                num_turns: 1,
                tool_calls: 1,
                cost_usd: Some(0.0),
                agent_ms: 0,
                commits: 0,
                files_changed: 0,
                dirty: false,
                verdict_json: "[]".into(),
                result_text: String::new(),
                envelope_json: envelope_json.into(),
                rl_five_hour: None,
                rl_seven_day: None,
                rl_five_hour_resets: None,
                rl_seven_day_resets: None,
                end_sha: String::new(),
                outputs_json: String::new(),
                session_id: String::new(),
                first_edit: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                early_signals: "[]".into(),
                early_near: "[]".into(),
            })
            .unwrap();
        };

        // investigate: asked a plain question, no changes: a success.
        let a1 = s.insert_attempt(&attempt("investigate")).unwrap();
        finish(a1, AttemptState::NeedsInput, &question("question"));

        // interview: the same.
        let a2 = s.insert_attempt(&attempt("interview")).unwrap();
        finish(a2, AttemptState::NeedsInput, &question("question"));

        // investigate that needed a different workflow, not a question it
        // asked: not what this counts.
        let a3 = s.insert_attempt(&attempt("investigate")).unwrap();
        finish(a3, AttemptState::NeedsInput, &question("workflow"));

        // code ending needs_input with a question: not an investigate or
        // interview role, so not counted as a success here.
        let a4 = s.insert_attempt(&attempt("code")).unwrap();
        finish(a4, AttemptState::NeedsInput, &question("question"));

        let stats = s.role_stats().unwrap();
        let find = |role: &str| stats.iter().find(|r| r.role == role).unwrap();

        let investigate = find("investigate");
        assert_eq!(investigate.attempts, 2);
        assert_eq!(
            investigate.succeeded, 1,
            "the plain question counts; the workflow one does not"
        );

        let interview = find("interview");
        assert_eq!(interview.attempts, 1);
        assert_eq!(interview.succeeded, 1);

        let code = find("code");
        assert_eq!(code.attempts, 1);
        assert_eq!(
            code.succeeded, 0,
            "needs_input on a role that is not investigate/interview is not a success"
        );
    }

    #[test]
    fn plugin_enabled_flag_reads_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        assert!(s.enabled_plugins().unwrap().is_empty(), "never recorded");
        s.set_plugin_enabled("notify", true, 100).unwrap();
        assert_eq!(
            s.enabled_plugins().unwrap(),
            ["notify".to_string()].into_iter().collect()
        );
        s.set_plugin_enabled("notify", false, 200).unwrap();
        assert!(s.enabled_plugins().unwrap().is_empty());
    }

    fn mk_project(s: &Store, name: &str) {
        s.create_project(&Project {
            name: name.to_string(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
    }

    #[test]
    fn deploy_targets_are_added_and_listed_alphabetically() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        assert!(s.deploy_targets("equitizr").unwrap().is_empty());
        assert!(s.deploy_target("equitizr", "prod").unwrap().is_none());

        let mut args = BTreeMap::new();
        args.insert("unit".to_string(), "equitizr.service".to_string());
        s.add_deploy_target(&DeployTarget {
            project: "equitizr".into(),
            name: "prod".into(),
            repo: "/repo".into(),
            scope: None,
            method: "deploy-user-service".into(),
            args: args.clone(),
            check_cmd: "systemctl is-active equitizr".into(),
            on_landing: true,
            smoke_url: Some("https://equitizr.example.com/".into()),
        })
        .unwrap();
        s.add_deploy_target(&DeployTarget {
            project: "equitizr".into(),
            name: "staging".into(),
            repo: "/repo".into(),
            scope: Some(r#"["web/"]"#.into()),
            method: "deploy-static".into(),
            args: BTreeMap::new(),
            check_cmd: "curl -f https://staging.example.com/health".into(),
            on_landing: false,
            smoke_url: None,
        })
        .unwrap();

        let targets = s.deploy_targets("equitizr").unwrap();
        assert_eq!(targets.len(), 2);
        // Alphabetical: "prod" before "staging".
        assert_eq!(targets[0].name, "prod");
        assert_eq!(targets[0].method, "deploy-user-service");
        assert_eq!(targets[0].args, args);
        assert!(targets[0].on_landing);
        assert_eq!(
            targets[0].smoke_url.as_deref(),
            Some("https://equitizr.example.com/")
        );
        assert_eq!(targets[1].name, "staging");
        assert_eq!(targets[1].scope.as_deref(), Some(r#"["web/"]"#));
        assert!(!targets[1].on_landing);
        assert_eq!(targets[1].smoke_url, None);

        let one = s.deploy_target("equitizr", "prod").unwrap().unwrap();
        assert_eq!(one.check_cmd, "systemctl is-active equitizr");

        // A duplicate (project, name) is refused.
        assert!(
            s.add_deploy_target(&DeployTarget {
                project: "equitizr".into(),
                name: "prod".into(),
                repo: "/repo".into(),
                scope: None,
                method: "deploy-command".into(),
                args: BTreeMap::new(),
                check_cmd: "true".into(),
                on_landing: false,
                smoke_url: None,
            })
            .is_err()
        );
    }

    #[test]
    fn a_deploy_target_is_updated_in_place_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");

        let mut args = BTreeMap::new();
        args.insert("unit".to_string(), "equitizr.service".to_string());
        s.add_deploy_target(&DeployTarget {
            project: "equitizr".into(),
            name: "prod".into(),
            repo: "/repo".into(),
            scope: None,
            method: "deploy-user-service".into(),
            args: args.clone(),
            check_cmd: "systemctl is-active equitizr".into(),
            on_landing: true,
            smoke_url: Some("https://equitizr.example.com/".into()),
        })
        .unwrap();

        // Updating a target that does not exist is refused.
        assert!(
            s.update_deploy_target(&DeployTarget {
                project: "equitizr".into(),
                name: "ghost".into(),
                repo: "/repo".into(),
                scope: None,
                method: "deploy-command".into(),
                args: BTreeMap::new(),
                check_cmd: "true".into(),
                on_landing: false,
                smoke_url: None,
            })
            .is_err()
        );

        // Update in place: the row stays under the same primary key.
        let mut t = s.deploy_target("equitizr", "prod").unwrap().unwrap();
        t.args.insert("host".to_string(), "box2".to_string());
        t.on_landing = false;
        s.update_deploy_target(&t).unwrap();

        let updated = s.deploy_target("equitizr", "prod").unwrap().unwrap();
        assert_eq!(
            updated.args.get("unit").map(String::as_str),
            Some("equitizr.service")
        );
        assert_eq!(updated.args.get("host").map(String::as_str), Some("box2"));
        assert!(!updated.on_landing);
        assert_eq!(updated.check_cmd, "systemctl is-active equitizr");
        assert_eq!(s.deploy_targets("equitizr").unwrap().len(), 1);

        // Removing an unknown target is refused; removing the real one
        // leaves no targets behind.
        assert!(s.remove_deploy_target("equitizr", "ghost").is_err());
        s.remove_deploy_target("equitizr", "prod").unwrap();
        assert!(s.deploy_targets("equitizr").unwrap().is_empty());
        assert!(s.deploy_target("equitizr", "prod").unwrap().is_none());
    }

    #[test]
    fn deploys_are_recorded_and_listed_newest_first_optionally_by_target() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        assert!(s.deploys("equitizr", None).unwrap().is_empty());

        let a = s
            .start_deploy("equitizr", "prod", "aaaaaaa", 100, None)
            .unwrap();
        let b = s
            .start_deploy("equitizr", "staging", "bbbbbbb", 200, None)
            .unwrap();
        s.finish_deploy(
            a,
            150,
            true,
            "active",
            None,
            "",
            Some(true),
            Some(r#"{"ok":true}"#),
        )
        .unwrap();
        s.finish_deploy(
            b,
            250,
            false,
            "connection refused",
            Some("aaaaaaa"),
            "the deploy of bbbbbbb failed its check and was rolled back to aaaaaaa",
            None,
            None,
        )
        .unwrap();

        let all = s.deploys("equitizr", None).unwrap();
        assert_eq!(all.len(), 2);
        // Newest first.
        assert_eq!(all[0].id, b);
        assert_eq!(all[0].target, "staging");
        assert_eq!(all[0].check_ok, Some(false));
        assert_eq!(all[0].rolled_back_to.as_deref(), Some("aaaaaaa"));
        assert_eq!(all[0].smoke_ok, None);
        assert_eq!(all[1].id, a);
        assert_eq!(all[1].check_ok, Some(true));
        assert_eq!(all[1].finished_at, Some(150));
        assert_eq!(all[1].smoke_ok, Some(true));
        assert_eq!(all[1].smoke_json.as_deref(), Some(r#"{"ok":true}"#));

        let prod_only = s.deploys("equitizr", Some("prod")).unwrap();
        assert_eq!(prod_only.len(), 1);
        assert_eq!(prod_only[0].id, a);
    }
}

#[cfg(test)]
mod column_tests {
    use super::*;

    fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("forge.db")).unwrap();
        (dir, store)
    }

    fn columns(store: &Store, table: &str) -> Vec<String> {
        let c = store.lock();
        let mut stmt = c.prepare(&format!("PRAGMA table_info({table})")).unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    #[test]
    fn the_column_lists_agree_with_the_schema() {
        let (_d, store) = open();
        for (table, cols) in [("tasks", TASK_COLUMNS), ("attempts", ATTEMPT_COLUMNS)] {
            let listed: Vec<String> = cols
                .iter()
                .map(|c| c.rsplit('.').next().unwrap().to_string())
                .collect();
            let actual = columns(&store, table);
            for c in &listed {
                assert!(
                    actual.contains(c),
                    "{table}: {c} is listed but not a column"
                );
            }
            for c in &actual {
                assert!(listed.contains(c), "{table}: column {c} is not in the list");
            }
        }
    }

    #[test]
    fn update_task_persists_every_field() {
        let (_d, store) = open();
        let mut t = Task {
            repo: "/r".into(),
            task: "do".into(),
            base_branch: "main".into(),
            model: "sonnet".into(),
            max_turns: 30,
            max_attempts: 2,
            timeout_secs: 60,
            state: TaskState::Queued,
            created_at: 1,
            workflow: "direct".into(),
            land: true,
            journal: true,
            context_enabled: true,
            ..Default::default()
        };
        t.id = store.insert_task(&t).unwrap();
        // Change every mutable field, then read it back.
        t.repo = "/elsewhere".into();
        t.task = "do more".into();
        t.base_branch = "dev".into();
        t.base_sha = "abc".into();
        t.branch = "forge/x".into();
        t.worktree = "/wt".into();
        t.model = "opus".into();
        t.max_turns = 99;
        t.max_attempts = 5;
        t.timeout_secs = 7;
        t.checks = vec!["true".into()];
        t.state = TaskState::Running;
        t.reason = "why".into();
        t.question_to = Some("alice".into());
        t.started_at = Some(2);
        t.finished_at = Some(3);
        t.pushed = true;
        t.worker_pid = Some(4);
        t.budget_usd = Some(1.5);
        t.allow_protected = true;
        t.workflow = "tdd".into();
        t.workflow_hash = "h".into();
        t.workflow_text = "text".into();
        t.actions_json = "[]".into();
        t.interface = "iface".into();
        t.show_checks = true;
        t.land = false;
        t.after = vec![7, 8];
        t.verify_base = "vb".into();
        t.retry_of = Some(9);
        t.journal = false;
        t.context = "ctx".into();
        t.context_enabled = false;
        t.resume_on_failure = true;
        t.plan = "plan".into();
        t.landed_sha = "abc123".into();
        t.journal_arm = "control".into();
        t.project = Some("proj".into());
        t.initiative = Some(11);
        store.update_task(&t).unwrap();
        let back = store.task(t.id).unwrap().unwrap();
        assert_eq!(format!("{back:?}"), format!("{t:?}"));
    }
}

impl Attempt {
    /// An attempt an agent made, as opposed to a row the kernel wrote
    /// about the task: the supervisor's rulings and the integrator's
    /// check runs are on the record but are not the agent's work, so a
    /// rule about "the last attempt" skips them.
    pub fn is_agent(&self) -> bool {
        self.step != "supervisor" && self.step != "integrate"
    }
}
