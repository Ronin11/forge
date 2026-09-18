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

mod attempts;
mod tasks;

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

/// A job's state: a run workflow's run, the way `TaskState` is a build
/// workflow's (see docs/JOBS.md, "Vocabulary"). `NeedsHuman` is a job's
/// `on_failure = "ask:*"` outcome, the job analogue of `TaskState::Blocked`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum JobState {
    #[default]
    Queued,
    Running,
    Ok,
    Failed,
    NeedsHuman,
    Dropped,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Ok => "ok",
            JobState::Failed => "failed",
            JobState::NeedsHuman => "needs_human",
            JobState::Dropped => "dropped",
        }
    }
}

impl TryFrom<&str> for JobState {
    type Error = std::io::Error;
    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        Ok(match s {
            "queued" => JobState::Queued,
            "running" => JobState::Running,
            "ok" => JobState::Ok,
            "failed" => JobState::Failed,
            "needs_human" => JobState::NeedsHuman,
            "dropped" => JobState::Dropped,
            other => {
                return Err(std::io::Error::other(format!(
                    "unknown job state {other:?}"
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
    /// The task in the customer's own words, for the day it was filed
    /// that way (`forge add --title`, or the concierge on a filed
    /// request): what `PortalDoc`'s "Done" line uses instead of deriving
    /// one from `task` (see docs/PORTAL.md). `None` for every task filed
    /// before this column, or never given one.
    pub title: Option<String>,
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
    /// When the task landed, `None` until then. Distinct from
    /// `finished_at`: a task verified before it had anywhere to land, or
    /// queued `--no-land`, sets `finished_at` at verification and only
    /// gets `landed_at` later, when a human's `forge land` (or the
    /// supervisor's own accept-and-land) actually lands it.
    pub landed_at: Option<i64>,
    /// Landed by a human's `forge land`, never by the supervisor's own
    /// automated landing: one of the human-attention signals (see
    /// `Store::human_attention_stats`).
    pub hand_landed: bool,
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
    /// The concierge decision that produced this task, raw JSON, when
    /// `forge ask` filed it (a `request`, a `need`, or the placeholder for
    /// `unclear`); `None` for a task filed any other way (see
    /// docs/INTAKE.md, "The front door is not the interview").
    pub concierge_json: Option<String>,
    /// The escalator's proposal, raw JSON (`task_ids`, `repetition`,
    /// `outcome`), on the placeholder task `forge ask` blocks when the
    /// concierge's decision names a `pattern`; `None` for every other task
    /// (see docs/INTAKE.md, "The escalator").
    pub proposal_json: Option<String>,
    /// How the proposal was answered, "yes" or "no"; `None` while it is
    /// still blocked.
    pub proposal_answer: Option<String>,
    /// The initiative a "yes" answer filed; `None` for a "no" or a still-open proposal.
    pub proposal_initiative: Option<i64>,
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

/// Human attention for one workflow version: what a person had to do for
/// its landed work, since minutes cannot be measured (docs/LATER.md, "Two
/// metrics the record can compute and does not"). Four signals, summed as
/// `events` and divided by `landed` to give `events_per_landed`:
/// `operator_answers` (decisions on this workflow's tasks with
/// `answered_by` other than `"supervisor"`), `hand_landed` (this
/// workflow's tasks landed by a human's `forge land`), `withdrawals`
/// (this workflow's tasks left `withdrawn`), and `hand_commits` (commits
/// not authored as Forge, on the base branch, between this workflow's
/// landings and the ones before them — the `task_hand_commits` cache,
/// summed the same way `WorkflowStat::repair_cost` sums `task_repair_cost`).
pub struct HumanAttentionStat {
    pub workflow: String,
    pub hash: String,
    pub landed: i64,
    pub operator_answers: i64,
    pub hand_landed: i64,
    pub withdrawals: i64,
    pub hand_commits: i64,
}

/// Human attention for one project: the same four signals as
/// `HumanAttentionStat`, over a project's tasks instead of one workflow
/// version's.
pub struct HumanAttentionProjectStat {
    pub project: String,
    pub landed: i64,
    pub operator_answers: i64,
    pub hand_landed: i64,
    pub withdrawals: i64,
    pub hand_commits: i64,
}

/// One landed task's time to live: how long the request took to go live
/// (docs/LATER.md, "Two metrics the record can compute and does not").
/// `secs` is `landed_at - created_at`, or, when a deploy ran on behalf of
/// this task, that deploy's `finished_at - created_at` instead — going
/// live means the deploy, not just the landing, once one is tied to the
/// task. `view::time_to_live` turns a scope's worth of these into the
/// median and 90th percentile, per workflow and per project.
pub struct TaskTtl {
    pub workflow: String,
    pub hash: String,
    pub project: Option<String>,
    pub secs: i64,
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

/// Whether `purpose` is the placeholder the migration and
/// `ensure_default_project`/`seed_project_for_repo` fill in for a
/// repository with no real purpose yet: `"Repository <path>."`. `forge
/// project set --purpose` is the only way to replace it; until then,
/// `forge project show` and the portal document treat it as though no
/// purpose were set at all (see docs/PROJECTS.md).
pub fn is_placeholder_purpose(purpose: &str) -> bool {
    match purpose.strip_prefix("Repository ") {
        Some(rest) => rest.trim_end_matches('.').starts_with('/'),
        None => false,
    }
}

/// What `forge project set` changes; a field left `None` keeps the
/// project's current value for that column. There is no way to clear a
/// column back to unset once set, which nothing here needs yet.
#[derive(Default, Debug, Clone)]
pub struct ProjectDefaults {
    /// A new purpose paragraph, replacing the migration's placeholder or
    /// any earlier text.
    pub purpose: Option<String>,
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
    /// Whether the `deploy-look` directive found the deployed page fit to
    /// show anyone, `None` when the target declared no smoke url or the
    /// screenshot smoke took was never produced for it to look at (see
    /// src/deploy_look.rs).
    pub look_ok: Option<bool>,
    /// `deploy-look`'s findings, as the JSON `[{"severity":"blocking"|
    /// "notable","finding":...}]` it returned.
    pub look_json: Option<String>,
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

/// One run of a `kind = "run"` workflow (see docs/JOBS.md, "Vocabulary"):
/// its trigger, its pinned workflow version, its state, and its cost.
/// `job_steps` and `job_effects` carry what it did; this row is what
/// `forge job list`/`show` and `finish_job` read and write.
#[derive(Default, Debug, Clone)]
pub struct Job {
    pub id: i64,
    pub project: String,
    pub workflow: String,
    /// Content hash of the workflow file this job ran under, mirroring
    /// `Task::workflow_hash`.
    pub workflow_hash: String,
    /// The project's landed commit this job ran the workflow's automation
    /// files at; empty for a job whose project has never landed anything.
    pub landed_sha: String,
    /// `workflows::TriggerOn::as_str()`: `manual`, `schedule`, `message`,
    /// `webhook`, or `event`.
    pub trigger_kind: String,
    /// The trigger's own value (a cron string, a contact, a webhook name,
    /// an event type), mirroring `workflows::Trigger::value()`; empty for
    /// a manual trigger.
    pub trigger_ref: String,
    pub state: JobState,
    /// `workflows::JobSource::as_str()`: `"repo"` when the workflow came
    /// from the project's own repository at `landed_sha`, `"catalog"` when
    /// it fell back to the operator's catalog (docs/JOBS.md, "Where an
    /// automation lives").
    pub workflow_source: String,
    /// Effects recorded, not performed: `forge job test`'s replay mode
    /// (docs/JOBS.md, "Verifying an automation").
    pub dry_run: bool,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub cost_usd: Option<f64>,
    /// The assertions' verdict, in the same shape as a task attempt's
    /// `verdict_json` (`checks::CheckResult` rows); empty until the job
    /// finishes.
    pub verdict_json: String,
}

/// One step of a job's run: one entry of the workflow's `steps`, whether
/// it was an operation or a directive (see docs/JOBS.md, "Steps").
#[derive(Default, Debug, Clone)]
pub struct JobStep {
    pub id: i64,
    pub job_id: i64,
    /// Position in the workflow's `steps` array, from 0.
    pub seq: i64,
    /// The action's name, e.g. `"draft-quote"`.
    pub action: String,
    /// `"operation"` or `"directive"`.
    pub kind: String,
    /// Set only for a directive step: the role's provider.
    pub provider: String,
    /// Set only for a directive step: the model that ran it.
    pub model: String,
    pub cost_usd: Option<f64>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// Set only for an operation step.
    pub exit_code: Option<i32>,
    /// Where the step's output is on disk, relative to the job's scratch
    /// directory.
    pub output_ref: String,
}

/// One effect a job's step performed on the world (see docs/JOBS.md,
/// "Effects"): a message sent, a row written, a file produced, an HTTP
/// call made. Logged whether or not the run was a dry run.
#[derive(Default, Debug, Clone)]
pub struct JobEffect {
    pub id: i64,
    pub job_id: i64,
    /// The step's `seq` that produced this effect.
    pub seq: i64,
    /// The operation's declared effect kind, e.g. `"message"`, `"row"`.
    pub kind: String,
    /// What the effect acted on: a phone number, a table row, a URL.
    pub target: String,
    /// A short human-readable description, what the portal shows per run.
    pub summary: String,
    /// True when the effect was only recorded, not performed (a dry run,
    /// e.g. `forge job test`'s fixture replay).
    pub dry_run: bool,
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

/// Jobs run in the last rolling 24h for one project, by outcome: what
/// `forge project show` and `forge stats`'s jobs section count separately
/// from tasks (docs/JOBS.md step 1d). `today` is every job started in the
/// window, whatever its current state; `ok`/`failed`/`needs_human` are
/// those of them that reached that state (a still-`queued` or `running`
/// job counts toward `today` alone).
#[derive(Default, Debug, Clone)]
pub struct JobStat {
    pub project: String,
    pub today: i64,
    pub ok: i64,
    pub failed: i64,
    pub needs_human: i64,
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
    // The deploy-look directive's verdict on a deploy's own row, beside
    // smoke_ok/smoke_json (see src/deploy_look.rs, docs/DEPLOY.md, "The
    // deploy look").
    "
ALTER TABLE deploys ADD COLUMN look_ok INTEGER;
ALTER TABLE deploys ADD COLUMN look_json TEXT;
",
    // The customer portal's access token (see docs/PORTAL.md, "What it
    // is"): a per-project link, minted by `forge project portal`. A
    // project can have more than one active token (minting again without
    // `--revoke` just adds one); `revoked_at` is how one stops working.
    "
CREATE TABLE portal_tokens (
  token TEXT PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(name),
  created_at INTEGER NOT NULL,
  revoked_at INTEGER
);
CREATE INDEX portal_tokens_project ON portal_tokens(project);
",
    // The concierge's decision, raw JSON, on the task it produced (see
    // docs/INTAKE.md, "The front door is not the interview"); an answer
    // records a decisions row instead, nothing here.
    "
ALTER TABLE tasks ADD COLUMN concierge_json TEXT;
",
    // A job is a run of a `kind = "run"` workflow: it starts from a
    // trigger, produces effects, and ends when they're verified — no
    // repository, no branch, no landing (see docs/JOBS.md, "The record").
    // `job_steps` and `job_effects` carry what happened; `jobs` is its own
    // state, cost and verdict.
    "
CREATE TABLE jobs (
  id INTEGER PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(name),
  workflow TEXT NOT NULL,
  workflow_hash TEXT NOT NULL DEFAULT '',
  landed_sha TEXT NOT NULL DEFAULT '',
  trigger_kind TEXT NOT NULL,
  trigger_ref TEXT NOT NULL DEFAULT '',
  state TEXT NOT NULL,
  dry_run INTEGER NOT NULL DEFAULT 0,
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  cost_usd REAL,
  verdict_json TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX jobs_project ON jobs(project, id);
CREATE INDEX jobs_state ON jobs(state, id);
CREATE TABLE job_steps (
  id INTEGER PRIMARY KEY,
  job_id INTEGER NOT NULL REFERENCES jobs(id),
  seq INTEGER NOT NULL,
  action TEXT NOT NULL,
  kind TEXT NOT NULL,
  provider TEXT NOT NULL DEFAULT '',
  model TEXT NOT NULL DEFAULT '',
  cost_usd REAL,
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  exit_code INTEGER,
  output_ref TEXT NOT NULL DEFAULT ''
);
CREATE INDEX job_steps_job ON job_steps(job_id, seq);
CREATE TABLE job_effects (
  id INTEGER PRIMARY KEY,
  job_id INTEGER NOT NULL REFERENCES jobs(id),
  seq INTEGER NOT NULL,
  kind TEXT NOT NULL,
  target TEXT NOT NULL,
  summary TEXT NOT NULL DEFAULT '',
  dry_run INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX job_effects_job ON job_effects(job_id, seq);
",
    // The escalator (see docs/INTAKE.md, "The escalator"): the pattern a
    // concierge decision named, on the placeholder task `forge ask` blocks
    // with the yes/no question, and how it was answered.
    "
ALTER TABLE tasks ADD COLUMN proposal_json TEXT;
ALTER TABLE tasks ADD COLUMN proposal_answer TEXT;
ALTER TABLE tasks ADD COLUMN proposal_initiative INTEGER;
",
    // The day tasks are filed in a customer's own words (see
    // docs/PORTAL.md): `forge add --title` and the concierge, on a filed
    // request, both set this; everything before it is NULL.
    "
ALTER TABLE tasks ADD COLUMN title TEXT;
",
    // Where a job's workflow was resolved from (see docs/JOBS.md, "Where
    // an automation lives"): the project's own repository at its pinned
    // commit, or the operator's catalog when the repository had no
    // workflow of that name there. `'catalog'` is the default so every
    // job recorded before this column existed reads as it always ran.
    "
ALTER TABLE jobs ADD COLUMN workflow_source TEXT NOT NULL DEFAULT 'catalog';
",
    // Two metrics the record did not compute on its own: human attention
    // (what a person had to do for a piece of landed work) and time to
    // live (how long a request took to go live). `landed_at` is when a
    // task actually landed — distinct from `finished_at`, which for a
    // task verified before it had anywhere to land (or queued `--no-land`)
    // is set at verification time, before a human's later `forge land`
    // (see docs/LATER.md's "Two metrics"). `hand_landed` is set only by
    // that hand path (`forge land`), never by the supervisor's own
    // automated landing. `task_hand_commits` caches, per landed task, the
    // hand commits (author not Forge's identity) on the base branch
    // between the previous landing on the same repository and this one's
    // `base_sha` — the git-level number `refresh_hand_commits` in view.rs
    // computes once and never revisits, since neither endpoint of that
    // range ever changes once this task has landed.
    "
ALTER TABLE tasks ADD COLUMN landed_at INTEGER;
ALTER TABLE tasks ADD COLUMN hand_landed INTEGER NOT NULL DEFAULT 0;
CREATE TABLE task_hand_commits (
  task_id INTEGER PRIMARY KEY REFERENCES tasks(id),
  hand_commits INTEGER NOT NULL,
  computed_at INTEGER NOT NULL
);
",
    // Indexes for the queries the portal and the initiative report run
    // per render (docs/REVIEW-2.md, item 5): tasks by project and by
    // initiative, decisions and deploys by task, backlog and repositories
    // by project.
    "
CREATE INDEX tasks_project ON tasks(project, id);
CREATE INDEX tasks_initiative ON tasks(initiative, id);
CREATE INDEX decisions_task ON decisions(task_id, id);
CREATE INDEX deploys_task ON deploys(task_id, id);
CREATE INDEX backlog_project ON backlog(project, id);
CREATE INDEX project_repos_project ON project_repos(project);
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
    "title",
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
    "concierge_json",
    "proposal_json",
    "proposal_answer",
    "proposal_initiative",
    "landed_at",
    "hand_landed",
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
        title: r.get("title")?,
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
        concierge_json: r.get("concierge_json")?,
        proposal_json: r.get("proposal_json")?,
        proposal_answer: r.get("proposal_answer")?,
        proposal_initiative: r.get("proposal_initiative")?,
        landed_at: r.get("landed_at")?,
        hand_landed: r.get::<_, i64>("hand_landed")? != 0,
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

const OP_COLUMNS: &[&str] = &[
    "id",
    "task_id",
    "seq",
    "name",
    "kernel",
    "started_at",
    "ms",
    "ok",
    "exit",
    "detail",
    "attempt_id",
    "output",
];

const DECISION_COLUMNS: &[&str] = &[
    "id",
    "task_id",
    "repo",
    "question",
    "answer",
    "created_at",
    "answered_by",
    "citations",
    "retry_id",
    "answered_for",
];

fn decision_from_row(r: &Row) -> rusqlite::Result<Decision> {
    Ok(Decision {
        id: r.get("id")?,
        task_id: r.get("task_id")?,
        repo: r.get("repo")?,
        question: r.get("question")?,
        answer: r.get("answer")?,
        created_at: r.get("created_at")?,
        answered_by: r.get("answered_by")?,
        citations: r.get("citations")?,
        retry_id: r.get("retry_id")?,
        answered_for: r.get("answered_for")?,
    })
}

const TASK_REF_COLUMNS: &[&str] = &["id", "task_id", "kind", "url", "label", "by", "created_at"];

fn task_ref_from_row(r: &Row) -> rusqlite::Result<TaskRef> {
    Ok(TaskRef {
        id: r.get("id")?,
        task_id: r.get("task_id")?,
        kind: r.get("kind")?,
        url: r.get("url")?,
        label: r.get("label")?,
        by: r.get("by")?,
        created_at: r.get("created_at")?,
    })
}

const ASSESSMENT_COLUMNS: &[&str] = &[
    "id",
    "task_id",
    "score",
    "findings_json",
    "model",
    "provider",
    "cost_usd",
    "created_at",
];

fn assessment_from_row(r: &Row) -> rusqlite::Result<Assessment> {
    Ok(Assessment {
        id: r.get("id")?,
        task_id: r.get("task_id")?,
        score: r.get("score")?,
        findings_json: r.get("findings_json")?,
        model: r.get("model")?,
        provider: r.get("provider")?,
        cost_usd: r.get("cost_usd")?,
        created_at: r.get("created_at")?,
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
        |r| Ok((r.get("repair_cost")?, r.get("computed_at")?)),
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
        |r| Ok((r.get("overlap_lines")?, r.get("removed_lines")?)),
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
        "SELECT DISTINCT provider, COALESCE(json_extract(inputs_json, '$.model'), '') AS model
         FROM attempts WHERE task_id = ?1 AND step = 'code'",
    )?;
    let rows = stmt.query_map(params![task_id], |r| {
        Ok((r.get("provider")?, r.get("model")?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `task_churn`'s cached row for `task_id`: `(added_lines, churned_lines,
/// computed_at)`, or `None` if it has never been computed.
fn churn_cache_query(c: &Connection, task_id: i64) -> Result<Option<(i64, i64, i64)>> {
    Ok(c.query_row(
        "SELECT added_lines, churned_lines, computed_at FROM task_churn WHERE task_id = ?1",
        params![task_id],
        |r| {
            Ok((
                r.get("added_lines")?,
                r.get("churned_lines")?,
                r.get("computed_at")?,
            ))
        },
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

/// `task_hand_commits`'s cached row for `task_id`: hand commits (author
/// not Forge's identity) on the base branch between the previous landing
/// on the same repository and this task's `base_sha`, or `None` if it has
/// never been computed. See `refresh_hand_commits` in view.rs.
fn hand_commits_cache_query(c: &Connection, task_id: i64) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT hand_commits FROM task_hand_commits WHERE task_id = ?1",
        params![task_id],
        |r| r.get(0),
    )
    .optional()?)
}

fn set_hand_commits_cache_query(
    c: &Connection,
    task_id: i64,
    hand_commits: i64,
    computed_at: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO task_hand_commits (task_id, hand_commits, computed_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(task_id) DO UPDATE SET
           hand_commits = excluded.hand_commits,
           computed_at = excluded.computed_at",
        params![task_id, hand_commits, computed_at],
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

    /// `task_hand_commits`'s cached row for `task_id`, if it has been computed.
    pub fn hand_commits_cache(&self, task_id: i64) -> Result<Option<i64>> {
        hand_commits_cache_query(&self.lock(), task_id)
    }

    /// Write (or overwrite) `task_id`'s cached hand-commit count.
    pub fn set_hand_commits_cache(
        &self,
        task_id: i64,
        hand_commits: i64,
        computed_at: i64,
    ) -> Result<()> {
        set_hand_commits_cache_query(&self.lock(), task_id, hand_commits, computed_at)
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
            "SELECT t.id AS id, t.state AS state,
                    COALESCE((SELECT SUM(cost_usd) FROM attempts a WHERE a.task_id=t.id),0) AS cost,
                    COALESCE(t.finished_at - t.started_at, 0) AS secs,
                    (SELECT COUNT(*) FROM attempts a WHERE a.task_id=t.id) AS attempts
             FROM tasks t WHERE t.workflow=?1 AND (?2 IS NULL OR t.workflow_hash=?2)
               AND t.state IN ('succeeded','failed','blocked','unverified')
               AND t.started_at IS NOT NULL
             ORDER BY t.id DESC LIMIT ?3",
        )?;
        let rows: Vec<(i64, String, f64, i64, i64)> = stmt
            .query_map(params![workflow, hash, limit as i64], |r| {
                Ok((
                    r.get("id")?,
                    r.get("state")?,
                    r.get("cost")?,
                    r.get("secs")?,
                    r.get("attempts")?,
                ))
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
                "SELECT t.workflow AS workflow, t.workflow_hash AS hash, COUNT(*) AS tasks,
                    SUM(t.state='succeeded') AS succeeded, SUM(t.state='failed') AS failed,
                    SUM(t.state='blocked') AS blocked, SUM(t.state='unverified') AS unverified,
                    COALESCE((SELECT SUM(a.cost_usd) FROM attempts a WHERE a.task_id IN (
                        SELECT id FROM tasks t2 WHERE t2.workflow=t.workflow AND t2.workflow_hash=t.workflow_hash
                          AND (?1 IS NULL OR t2.project = ?1) AND (?2 IS NULL OR t2.initiative = ?2)
                    )), 0) AS cost,
                    COALESCE((SELECT COUNT(*) FROM attempts a WHERE a.task_id IN (
                        SELECT id FROM tasks t2 WHERE t2.workflow=t.workflow AND t2.workflow_hash=t.workflow_hash
                          AND (?1 IS NULL OR t2.project = ?1) AND (?2 IS NULL OR t2.initiative = ?2)
                    )), 0) AS attempts,
                    SUM(t.landed_sha != '') AS landed,
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
                    )) AS broke_base,
                    SUM(t.landed_sha != '' AND EXISTS (
                        SELECT 1 FROM task_refs r WHERE r.kind = 'repairs' AND r.url = 'forge://task/' || t.id
                    )) AS repaired
             FROM tasks t WHERE t.state IN ('succeeded','failed','blocked','unverified') AND t.started_at IS NOT NULL
               AND (?1 IS NULL OR t.project = ?1) AND (?2 IS NULL OR t.initiative = ?2)
             GROUP BY t.workflow, t.workflow_hash ORDER BY t.workflow, t.workflow_hash",
            )?;
            let rows = stmt.query_map(params![scope.project, scope.initiative], |r| {
                Ok(WorkflowStat {
                    workflow: r.get("workflow")?,
                    hash: r.get("hash")?,
                    tasks: r.get("tasks")?,
                    succeeded: r.get("succeeded")?,
                    failed: r.get("failed")?,
                    blocked: r.get("blocked")?,
                    unverified: r.get("unverified")?,
                    cost: r.get("cost")?,
                    attempts: r.get("attempts")?,
                    landed: r.get("landed")?,
                    broke_base: r.get("broke_base")?,
                    repaired: r.get("repaired")?,
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

    /// Time to live for this scope's landed tasks (see `TaskTtl`): one row
    /// per landed task that already has an answer — its own `landed_at`,
    /// or, when an on-landing deploy ran on its behalf and has finished, that
    /// deploy's `finished_at`. A landed task with a deploy still running
    /// (`finished_at` still `None`) is left out until it finishes, rather
    /// than counted early against its mere landing.
    pub fn task_ttls(&self, scope: &StatsFilter) -> Result<Vec<TaskTtl>> {
        let mut out = Vec::new();
        for t in self.landed_tasks(scope)? {
            let deploys = self.deploys_for_task(t.id)?;
            let end = if let Some(d) = deploys.first() {
                d.finished_at
            } else {
                t.landed_at
            };
            if let Some(end) = end {
                out.push(TaskTtl {
                    workflow: t.workflow.clone(),
                    hash: t.workflow_hash.clone(),
                    project: t.project.clone(),
                    secs: end - t.created_at,
                });
            }
        }
        Ok(out)
    }

    /// Human attention per workflow version: what a person had to do for
    /// its landed work (see `HumanAttentionStat`). One row per workflow +
    /// hash with at least one task in scope, whatever its state — unlike
    /// `workflow_stats`, a workflow whose only tasks were withdrawn still
    /// gets a row here, since a withdrawal is itself a human-attention
    /// signal.
    pub fn human_attention_stats(&self, scope: &StatsFilter) -> Result<Vec<HumanAttentionStat>> {
        let mut stats = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT t.workflow AS workflow, t.workflow_hash AS hash, SUM(t.landed_sha != '') AS landed,
                    COALESCE((SELECT COUNT(*) FROM decisions d JOIN tasks dt ON dt.id = d.task_id
                        WHERE dt.workflow = t.workflow AND dt.workflow_hash = t.workflow_hash
                          AND d.answered_by != 'supervisor'
                          AND (?1 IS NULL OR dt.project = ?1) AND (?2 IS NULL OR dt.initiative = ?2)
                    ), 0) AS operator_answers,
                    SUM(t.hand_landed) AS hand_landed, SUM(t.state = 'withdrawn') AS withdrawals
             FROM tasks t WHERE (?1 IS NULL OR t.project = ?1) AND (?2 IS NULL OR t.initiative = ?2)
             GROUP BY t.workflow, t.workflow_hash ORDER BY t.workflow, t.workflow_hash",
            )?;
            let rows = stmt.query_map(params![scope.project, scope.initiative], |r| {
                Ok(HumanAttentionStat {
                    workflow: r.get("workflow")?,
                    hash: r.get("hash")?,
                    landed: r.get("landed")?,
                    operator_answers: r.get("operator_answers")?,
                    hand_landed: r.get("hand_landed")?,
                    withdrawals: r.get("withdrawals")?,
                    hand_commits: 0,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<HumanAttentionStat>>>()?
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
            if let Some(hand_commits) = hand_commits_cache_query(&c, t.id)? {
                w.hand_commits += hand_commits;
            }
        }
        Ok(stats)
    }

    /// Outcomes per workflow step.
    pub fn step_stats(&self, scope: &StatsFilter) -> Result<Vec<StepStat>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.workflow AS workflow, a.step AS step, COUNT(*) AS attempts,
                    SUM(a.state='succeeded') AS succeeded, SUM(a.state='agent_failed') AS agent_failed,
                    SUM(a.state='checks_failed') AS checks_failed, SUM(a.state='needs_input') AS needs_input,
                    AVG(a.num_turns) AS mean_turns, COALESCE(SUM(a.cost_usd),0) AS cost, AVG(a.agent_ms) AS mean_ms,
                    AVG(a.first_edit) AS mean_first_edit, AVG(a.input_tokens) AS mean_input_tokens
             FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.state != 'running'
               AND (?1 IS NULL OR t.project = ?1) AND (?2 IS NULL OR t.initiative = ?2)
             GROUP BY t.workflow, a.step ORDER BY t.workflow, a.step",
        )?;
        let rows = stmt.query_map(params![scope.project, scope.initiative], |r| {
            Ok(StepStat {
                workflow: r.get("workflow")?,
                step: r.get("step")?,
                attempts: r.get("attempts")?,
                succeeded: r.get("succeeded")?,
                agent_failed: r.get("agent_failed")?,
                checks_failed: r.get("checks_failed")?,
                needs_input: r.get("needs_input")?,
                mean_turns: r.get("mean_turns")?,
                cost: r.get("cost")?,
                mean_ms: r.get("mean_ms")?,
                mean_first_edit: r.get("mean_first_edit")?,
                mean_input_tokens: r.get("mean_input_tokens")?,
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
                COUNT(*) AS attempts, SUM(a.state='succeeded') AS succeeded,
                AVG(a.num_turns) AS mean_turns, AVG(a.first_edit) AS mean_first_edit,
                COALESCE(AVG(a.cost_usd), 0) AS mean_cost_usd
             FROM attempts a
             WHERE a.step = 'code' AND a.attempt_no > 1 AND a.state != 'running'
             GROUP BY has_journal",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(JournalStat {
                has_journal: r.get("has_journal")?,
                attempts: r.get("attempts")?,
                succeeded: r.get("succeeded")?,
                mean_turns: r.get("mean_turns")?,
                mean_first_edit: r.get("mean_first_edit")?,
                mean_cost_usd: r.get("mean_cost_usd")?,
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
                "SELECT a.step AS role, a.provider AS provider, COALESCE(json_extract(a.inputs_json, '$.model'), '') AS attempt_model,
                    COUNT(*) AS attempts,
                    SUM(CASE
                        WHEN a.state='succeeded' THEN 1
                        WHEN a.step IN ('investigate', 'interview') AND a.state='needs_input'
                            AND a.envelope_json != '' AND json_valid(a.envelope_json)
                            AND COALESCE(json_extract(a.envelope_json, '$.needs_input.kind'), 'question') = 'question'
                        THEN 1
                        ELSE 0
                    END) AS succeeded,
                    AVG(a.num_turns) AS mean_turns, COALESCE(AVG(a.cost_usd), 0) AS mean_cost_usd, AVG(a.agent_ms) AS mean_ms,
                    COUNT(DISTINCT CASE WHEN t.landed_sha != '' THEN t.id END) AS landed,
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
                    ) THEN t.id END) AS broke_base
             FROM attempts a JOIN tasks t ON t.id = a.task_id
             WHERE a.state != 'running'
             GROUP BY a.step, a.provider, attempt_model
             ORDER BY a.step, a.provider, attempt_model",
            )?;
            let rows = stmt.query_map([], |r| {
                let role: String = r.get("role")?;
                let landed: i64 = r.get("landed")?;
                let broke_base: i64 = r.get("broke_base")?;
                let is_code = role == "code";
                Ok(RoleStat {
                    role,
                    provider: r.get("provider")?,
                    model: r.get("attempt_model")?,
                    attempts: r.get("attempts")?,
                    succeeded: r.get("succeeded")?,
                    mean_turns: r.get("mean_turns")?,
                    mean_cost_usd: r.get("mean_cost_usd")?,
                    mean_ms: r.get("mean_ms")?,
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
            "SELECT t.id AS id, t.state AS state, datetime(t.created_at,'unixepoch','localtime') AS created,
                    t.repo AS repo, t.task AS task,
                    (SELECT COUNT(*) FROM attempts a WHERE a.task_id=t.id) AS attempts,
                    (SELECT COALESCE(SUM(cost_usd),0) FROM attempts a WHERE a.task_id=t.id) AS cost,
                    t.workflow AS workflow, t.created_at AS created_at, t.finished_at AS finished_at,
                    t.project AS project, t.initiative AS initiative
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
                    id: r.get("id")?,
                    state: r.get("state")?,
                    created: r.get("created")?,
                    repo: r.get("repo")?,
                    task: r.get("task")?,
                    attempts: r.get("attempts")?,
                    cost: r.get("cost")?,
                    workflow: r.get("workflow")?,
                    created_at: r.get("created_at")?,
                    finished_at: r.get("finished_at")?,
                    project: r.get("project")?,
                    initiative: r.get("initiative")?,
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
            "SELECT {} FROM decisions d WHERE d.task_id IN ({placeholders})
             ORDER BY d.id",
            DECISION_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), decision_from_row)?;
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
        let cols = DECISION_COLUMNS
            .iter()
            .map(|c| format!("d.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = c.prepare(&format!(
            "SELECT {cols}
             FROM decisions d JOIN tasks t ON t.id = d.task_id
             WHERE (?1 IS NULL OR d.repo = ?1)
               AND (?2 IS NULL OR t.project = ?2)
               AND (?3 IS NULL OR t.initiative = ?3)
             ORDER BY d.id DESC"
        ))?;
        let rows = stmt.query_map(params![q.repo, q.project, q.initiative], decision_from_row)?;
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
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM task_refs WHERE task_id = ?1 ORDER BY id",
            TASK_REF_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![task_id], task_ref_from_row)?;
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
                &format!(
                    "SELECT {} FROM projects WHERE name=?1",
                    PROJECT_COLUMNS.join(", ")
                ),
                params![name],
                project_from_row,
            )
            .optional()?)
    }

    /// Every project, alphabetically.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM projects ORDER BY name",
            PROJECT_COLUMNS.join(", ")
        ))?;
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
                purpose = COALESCE(?2, purpose),
                workflow = COALESCE(?3, workflow),
                per_task_usd = COALESCE(?4, per_task_usd),
                per_initiative_usd = COALESCE(?5, per_initiative_usd),
                supervisor_model = COALESCE(?6, supervisor_model),
                supervisor_per_lineage = COALESCE(?7, supervisor_per_lineage),
                protected_json = COALESCE(?8, protected_json),
                role_providers_json = COALESCE(?9, role_providers_json)
             WHERE name=?1",
            params![
                name,
                d.purpose,
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
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM backlog WHERE project=?1 ORDER BY id",
            BACKLOG_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![project], backlog_from_row)?;
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

    /// Mint a fresh portal token for a project (see docs/PORTAL.md, "What
    /// it is"): `forge project portal` generates the token text itself
    /// (32 random bytes, hex-encoded) and records it here.
    pub fn create_portal_token(&self, project: &str, token: &str, at: i64) -> Result<()> {
        self.lock().execute(
            "INSERT INTO portal_tokens (token, project, created_at) VALUES (?1, ?2, ?3)",
            params![token, project, at],
        )?;
        Ok(())
    }

    /// Revoke every currently-active token on a project (`forge project
    /// portal --revoke`). Returns how many were revoked.
    pub fn revoke_portal_tokens(&self, project: &str, at: i64) -> Result<usize> {
        Ok(self.lock().execute(
            "UPDATE portal_tokens SET revoked_at=?2 WHERE project=?1 AND revoked_at IS NULL",
            params![project, at],
        )?)
    }

    /// The project an active (unrevoked) portal token opens, if any: how
    /// `forge project resolve-token` resolves `/p/<token>` for the portal
    /// server (see docs/PORTAL.md).
    pub fn portal_token_project(&self, token: &str) -> Result<Option<String>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT project FROM portal_tokens WHERE token=?1 AND revoked_at IS NULL",
                params![token],
                |r| r.get(0),
            )
            .optional()?)
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
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM project_repos WHERE project=?1 ORDER BY repo",
            PROJECT_REPO_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![project], project_repo_from_row)?;
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
                &format!(
                    "SELECT {} FROM initiatives WHERE id=?1",
                    INITIATIVE_COLUMNS.join(", ")
                ),
                params![id],
                initiative_from_row,
            )
            .optional()?)
    }

    /// Every initiative, oldest first; only `project`'s when given.
    pub fn list_initiatives(&self, project: Option<&str>) -> Result<Vec<Initiative>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM initiatives WHERE ?1 IS NULL OR project = ?1 ORDER BY id",
            INITIATIVE_COLUMNS.join(", ")
        ))?;
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
            "SELECT SUM(state='queued') AS queued, SUM(state='running') AS running,
                    SUM(state='succeeded') AS succeeded, SUM(state='failed') AS failed,
                    SUM(state='unverified') AS unverified, SUM(state='blocked') AS blocked,
                    SUM(state='withdrawn') AS withdrawn,
                    COALESCE((SELECT SUM(a.cost_usd) FROM attempts a WHERE a.task_id IN
                        (SELECT id FROM tasks WHERE project=?1)), 0) AS cost
             FROM tasks WHERE project=?1",
            params![project],
            |r| {
                Ok(ProjectTaskStats {
                    queued: r.get::<_, Option<i64>>("queued")?.unwrap_or(0),
                    running: r.get::<_, Option<i64>>("running")?.unwrap_or(0),
                    succeeded: r.get::<_, Option<i64>>("succeeded")?.unwrap_or(0),
                    failed: r.get::<_, Option<i64>>("failed")?.unwrap_or(0),
                    unverified: r.get::<_, Option<i64>>("unverified")?.unwrap_or(0),
                    blocked: r.get::<_, Option<i64>>("blocked")?.unwrap_or(0),
                    withdrawn: r.get::<_, Option<i64>>("withdrawn")?.unwrap_or(0),
                    cost: r.get("cost")?,
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
            "SELECT t.project AS project, COUNT(*) AS tasks, SUM(t.landed_sha != '') AS landed,
                    COALESCE((SELECT SUM(a.cost_usd) FROM attempts a WHERE a.task_id IN (SELECT id FROM tasks t2 WHERE t2.project=t.project)), 0) AS cost,
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
                    )) AS broke_base
             FROM tasks t WHERE t.project IS NOT NULL AND t.state IN ('succeeded','failed','blocked','unverified') AND t.started_at IS NOT NULL
             GROUP BY t.project ORDER BY t.project",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ProjectStat {
                project: r.get("project")?,
                tasks: r.get("tasks")?,
                landed: r.get("landed")?,
                cost: r.get("cost")?,
                broke_base: r.get("broke_base")?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Human attention per project: what a person had to do for its
    /// landed work (see `HumanAttentionStat`), same scoping rule as
    /// `project_stats` (only every project's own tasks, whatever their
    /// state — a project with only withdrawn tasks still gets a row).
    pub fn human_attention_project_stats(&self) -> Result<Vec<HumanAttentionProjectStat>> {
        let mut stats = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT t.project AS project, SUM(t.landed_sha != '') AS landed,
                    COALESCE((SELECT COUNT(*) FROM decisions d JOIN tasks dt ON dt.id = d.task_id
                        WHERE dt.project = t.project AND d.answered_by != 'supervisor'
                    ), 0) AS operator_answers,
                    SUM(t.hand_landed) AS hand_landed, SUM(t.state = 'withdrawn') AS withdrawals
             FROM tasks t WHERE t.project IS NOT NULL
             GROUP BY t.project ORDER BY t.project",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(HumanAttentionProjectStat {
                    project: r.get("project")?,
                    landed: r.get("landed")?,
                    operator_answers: r.get("operator_answers")?,
                    hand_landed: r.get("hand_landed")?,
                    withdrawals: r.get("withdrawals")?,
                    hand_commits: 0,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<HumanAttentionProjectStat>>>()?
        };
        let landed = self.landed_tasks(&StatsFilter::default())?;
        let c = self.lock();
        for t in &landed {
            let Some(project) = &t.project else {
                continue;
            };
            let Some(p) = stats.iter_mut().find(|p| &p.project == project) else {
                continue;
            };
            if let Some(hand_commits) = hand_commits_cache_query(&c, t.id)? {
                p.hand_commits += hand_commits;
            }
        }
        Ok(stats)
    }

    /// One project's jobs in the last rolling 24h, by outcome: what `forge
    /// project show` counts separately from its task rollup (see
    /// `JobStat`, docs/JOBS.md step 1d).
    pub fn project_job_stats(&self, project: &str, since: i64) -> Result<JobStat> {
        Ok(self.lock().query_row(
            "SELECT COUNT(*) AS today, SUM(state='ok') AS ok, SUM(state='failed') AS failed, SUM(state='needs_human') AS needs_human
             FROM jobs WHERE project=?1 AND started_at >= ?2",
            params![project, since],
            |r| {
                Ok(JobStat {
                    project: project.to_string(),
                    today: r.get("today")?,
                    ok: r.get::<_, Option<i64>>("ok")?.unwrap_or(0),
                    failed: r.get::<_, Option<i64>>("failed")?.unwrap_or(0),
                    needs_human: r.get::<_, Option<i64>>("needs_human")?.unwrap_or(0),
                })
            },
        )?)
    }

    /// Every project with a job in the last rolling 24h, by outcome: what
    /// `forge stats` adds as its jobs section when it is not itself scoped
    /// to one project or initiative (see `JobStat`).
    pub fn job_stats(&self, since: i64) -> Result<Vec<JobStat>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT project AS project, COUNT(*) AS today, SUM(state='ok') AS ok, SUM(state='failed') AS failed, SUM(state='needs_human') AS needs_human
             FROM jobs WHERE started_at >= ?1 GROUP BY project ORDER BY project",
        )?;
        let rows = stmt.query_map(params![since], |r| {
            Ok(JobStat {
                project: r.get("project")?,
                today: r.get("today")?,
                ok: r.get::<_, Option<i64>>("ok")?.unwrap_or(0),
                failed: r.get::<_, Option<i64>>("failed")?.unwrap_or(0),
                needs_human: r.get::<_, Option<i64>>("needs_human")?.unwrap_or(0),
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
                &format!(
                    "SELECT {} FROM deploy_targets WHERE project=?1 AND name=?2",
                    DEPLOY_TARGET_COLUMNS.join(", ")
                ),
                params![project, name],
                deploy_target_from_row,
            )
            .optional()?)
    }

    /// A project's deploy targets, alphabetically.
    pub fn deploy_targets(&self, project: &str) -> Result<Vec<DeployTarget>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM deploy_targets WHERE project=?1 ORDER BY name",
            DEPLOY_TARGET_COLUMNS.join(", ")
        ))?;
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
    /// rolled back to (if it did), why, the smoke operation's verdict when
    /// the target declared a smoke url and the check passed for it to run
    /// (`None`, `None` otherwise), and `deploy-look`'s verdict on the same
    /// terms (`None`, `None` when it never ran).
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
        look_ok: Option<bool>,
        look_json: Option<&str>,
    ) -> Result<()> {
        self.lock().execute(
            "UPDATE deploys SET finished_at=?2, check_ok=?3, check_output=?4, rolled_back_to=?5, reason=?6, smoke_ok=?7, smoke_json=?8, look_ok=?9, look_json=?10
             WHERE id=?1",
            params![
                id,
                at,
                check_ok,
                check_output,
                rolled_back_to,
                reason,
                smoke_ok,
                smoke_json,
                look_ok,
                look_json
            ],
        )?;
        Ok(())
    }

    /// A project's deploys, newest first; only `target`'s when given: what
    /// `forge deploy log` shows.
    pub fn deploys(&self, project: &str, target: Option<&str>) -> Result<Vec<Deploy>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM deploys WHERE project=?1 AND (?2 IS NULL OR target=?2) ORDER BY id DESC",
            DEPLOY_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![project, target], deploy_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// A task's deploys, newest first: the on-landing targets it triggered
    /// when it landed (see docs/DEPLOY.md, "When a deploy runs"). What
    /// `forge show`, `forge trace --json`, and the web task view list
    /// under the task.
    pub fn deploys_for_task(&self, task_id: i64) -> Result<Vec<Deploy>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM deploys WHERE task_id=?1 ORDER BY id DESC",
            DEPLOY_COLUMNS.join(", ")
        ))?;
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
                &format!(
                    "SELECT {} FROM assessments WHERE task_id=?1 ORDER BY id DESC LIMIT 1",
                    ASSESSMENT_COLUMNS.join(", ")
                ),
                params![task_id],
                assessment_from_row,
            )
            .optional()?)
    }

    /// Record a job starting. Returns its id; `finish_job` completes it,
    /// `append_job_step`/`append_job_effect` record what it did along the
    /// way (see docs/JOBS.md, "The record"), and `src/job.rs` is the
    /// executor that calls all four.
    pub fn create_job(&self, j: &Job) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO jobs (project, workflow, workflow_hash, landed_sha, trigger_kind, trigger_ref, state, workflow_source, dry_run, started_at, finished_at, cost_usd, verdict_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                j.project,
                j.workflow,
                j.workflow_hash,
                j.landed_sha,
                j.trigger_kind,
                j.trigger_ref,
                j.state.as_str(),
                j.workflow_source,
                j.dry_run,
                j.started_at,
                j.finished_at,
                j.cost_usd,
                j.verdict_json,
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Record one step of a job's run. Returns its id; see `create_job`.
    pub fn append_job_step(&self, s: &JobStep) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO job_steps (job_id, seq, action, kind, provider, model, cost_usd, started_at, finished_at, exit_code, output_ref)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                s.job_id,
                s.seq,
                s.action,
                s.kind,
                s.provider,
                s.model,
                s.cost_usd,
                s.started_at,
                s.finished_at,
                s.exit_code,
                s.output_ref,
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Record one effect a job's step performed on the world. Returns its
    /// id; see `create_job`.
    pub fn append_job_effect(&self, e: &JobEffect) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO job_effects (job_id, seq, kind, target, summary, dry_run)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![e.job_id, e.seq, e.kind, e.target, e.summary, e.dry_run],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Record a job's outcome: its final state, cost and assertions'
    /// verdict; see `create_job`.
    pub fn finish_job(
        &self,
        id: i64,
        at: i64,
        state: JobState,
        cost_usd: Option<f64>,
        verdict_json: &str,
    ) -> Result<()> {
        self.lock().execute(
            "UPDATE jobs SET state=?2, finished_at=?3, cost_usd=?4, verdict_json=?5 WHERE id=?1",
            params![id, state.as_str(), at, cost_usd, verdict_json],
        )?;
        Ok(())
    }

    /// One job by id.
    pub fn job(&self, id: i64) -> Result<Option<Job>> {
        Ok(self
            .lock()
            .query_row(
                &format!("SELECT {} FROM jobs WHERE id=?1", JOB_COLUMNS.join(", ")),
                params![id],
                job_from_row,
            )
            .optional()?)
    }

    /// Jobs, newest first, optionally narrowed to one project and/or one
    /// state: what `forge job list` shows.
    pub fn jobs(&self, project: Option<&str>, state: Option<JobState>) -> Result<Vec<Job>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM jobs WHERE (?1 IS NULL OR project=?1) AND (?2 IS NULL OR state=?2) ORDER BY id DESC",
            JOB_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![project, state.map(JobState::as_str)], job_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// One job's steps, in the order they ran.
    pub fn job_steps(&self, job_id: i64) -> Result<Vec<JobStep>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM job_steps WHERE job_id=?1 ORDER BY seq",
            JOB_STEP_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![job_id], job_step_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// One job's effects, in the order they happened.
    pub fn job_effects(&self, job_id: i64) -> Result<Vec<JobEffect>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM job_effects WHERE job_id=?1 ORDER BY seq",
            JOB_EFFECT_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![job_id], job_effect_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// A project's job effects across every one of its jobs, newest first:
    /// what `forge job log` shows.
    pub fn job_effects_for_project(&self, project: &str) -> Result<Vec<JobEffect>> {
        let c = self.lock();
        let cols = JOB_EFFECT_COLUMNS
            .iter()
            .map(|c| format!("job_effects.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = c.prepare(&format!(
            "SELECT {cols}
             FROM job_effects JOIN jobs ON jobs.id = job_effects.job_id
             WHERE jobs.project=?1 ORDER BY job_effects.id DESC",
        ))?;
        let rows = stmt.query_map(params![project], job_effect_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Queued jobs, oldest first: what the worker's claim loop considers,
    /// alongside `queued_unblocked`'s tasks (see `claim_next_job`).
    pub fn queued_jobs(&self) -> Result<Vec<Job>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM jobs WHERE state='queued' ORDER BY id",
            JOB_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map([], job_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The oldest queued job the worker can claim right now, same shape as
    /// `claim_next` for tasks: a job carries no provider or initiative
    /// hold yet (it runs no directive step), so the first one found is
    /// always claimable.
    pub fn claim_next_job(&self) -> Result<Option<Job>> {
        let id: Option<i64> = self
            .lock()
            .query_row(
                "UPDATE jobs SET state='running'
                 WHERE id = (SELECT id FROM jobs WHERE state='queued' ORDER BY id LIMIT 1)
                 RETURNING id",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match id {
            Some(id) => self.job(id),
            None => Ok(None),
        }
    }

    /// How many of `project`'s runs of `workflow` started at or after
    /// `since`, dry runs excluded: `Limits.per_day` is checked against it
    /// by `job::start` (docs/JOBS.md, "Limits").
    pub fn jobs_started_since(&self, project: &str, workflow: &str, since: i64) -> Result<i64> {
        Ok(self.lock().query_row(
            "SELECT COUNT(*) FROM jobs WHERE project=?1 AND workflow=?2 AND dry_run=0 AND started_at >= ?3",
            params![project, workflow, since],
            |r| r.get(0),
        )?)
    }

    /// Put a running job back in the queue: the worker aborted with it
    /// still in flight (see `worker::work`'s double-signal abort, which
    /// does the same for a running task's `requeue`).
    pub fn requeue_job(&self, id: i64) -> Result<()> {
        self.lock().execute(
            "UPDATE jobs SET state='queued' WHERE id=?1 AND state='running'",
            params![id],
        )?;
        Ok(())
    }
}

const JOB_COLUMNS: &[&str] = &[
    "id",
    "project",
    "workflow",
    "workflow_hash",
    "landed_sha",
    "trigger_kind",
    "trigger_ref",
    "state",
    "dry_run",
    "started_at",
    "finished_at",
    "cost_usd",
    "verdict_json",
    "workflow_source",
];

const JOB_STEP_COLUMNS: &[&str] = &[
    "id",
    "job_id",
    "seq",
    "action",
    "kind",
    "provider",
    "model",
    "cost_usd",
    "started_at",
    "finished_at",
    "exit_code",
    "output_ref",
];

const JOB_EFFECT_COLUMNS: &[&str] = &[
    "id", "job_id", "seq", "kind", "target", "summary", "dry_run",
];

fn job_from_row(r: &Row) -> rusqlite::Result<Job> {
    Ok(Job {
        id: r.get("id")?,
        project: r.get("project")?,
        workflow: r.get("workflow")?,
        workflow_hash: r.get("workflow_hash")?,
        landed_sha: r.get("landed_sha")?,
        trigger_kind: r.get("trigger_kind")?,
        trigger_ref: r.get("trigger_ref")?,
        state: conv(
            r,
            "state",
            JobState::try_from(r.get::<_, String>("state")?.as_str()),
        )?,
        dry_run: r.get("dry_run")?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
        cost_usd: r.get("cost_usd")?,
        verdict_json: r.get("verdict_json")?,
        workflow_source: r.get("workflow_source")?,
    })
}

fn job_step_from_row(r: &Row) -> rusqlite::Result<JobStep> {
    Ok(JobStep {
        id: r.get("id")?,
        job_id: r.get("job_id")?,
        seq: r.get("seq")?,
        action: r.get("action")?,
        kind: r.get("kind")?,
        provider: r.get("provider")?,
        model: r.get("model")?,
        cost_usd: r.get("cost_usd")?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
        exit_code: r.get("exit_code")?,
        output_ref: r.get("output_ref")?,
    })
}

fn job_effect_from_row(r: &Row) -> rusqlite::Result<JobEffect> {
    Ok(JobEffect {
        id: r.get("id")?,
        job_id: r.get("job_id")?,
        seq: r.get("seq")?,
        kind: r.get("kind")?,
        target: r.get("target")?,
        summary: r.get("summary")?,
        dry_run: r.get("dry_run")?,
    })
}

const DEPLOY_TARGET_COLUMNS: &[&str] = &[
    "project",
    "name",
    "repo",
    "scope_json",
    "method",
    "args_json",
    "check_cmd",
    "on_landing",
    "smoke_url",
];

const DEPLOY_COLUMNS: &[&str] = &[
    "id",
    "project",
    "target",
    "sha",
    "started_at",
    "finished_at",
    "check_ok",
    "check_output",
    "rolled_back_to",
    "reason",
    "task_id",
    "smoke_ok",
    "smoke_json",
    "look_ok",
    "look_json",
];

fn deploy_target_from_row(r: &Row) -> rusqlite::Result<DeployTarget> {
    Ok(DeployTarget {
        project: r.get("project")?,
        name: r.get("name")?,
        repo: r.get("repo")?,
        scope: r.get("scope_json")?,
        method: r.get("method")?,
        args: serde_json::from_str(&r.get::<_, String>("args_json")?).unwrap_or_default(),
        check_cmd: r.get("check_cmd")?,
        on_landing: r.get("on_landing")?,
        smoke_url: r.get("smoke_url")?,
    })
}

fn deploy_from_row(r: &Row) -> rusqlite::Result<Deploy> {
    Ok(Deploy {
        id: r.get("id")?,
        project: r.get("project")?,
        target: r.get("target")?,
        sha: r.get("sha")?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
        check_ok: r.get("check_ok")?,
        check_output: r.get("check_output")?,
        rolled_back_to: r.get("rolled_back_to")?,
        reason: r.get("reason")?,
        task_id: r.get("task_id")?,
        smoke_ok: r.get("smoke_ok")?,
        smoke_json: r.get("smoke_json")?,
        look_ok: r.get("look_ok")?,
        look_json: r.get("look_json")?,
    })
}

const PROJECT_COLUMNS: &[&str] = &[
    "name",
    "purpose",
    "created_at",
    "workflow",
    "per_task_usd",
    "per_initiative_usd",
    "supervisor_model",
    "supervisor_per_lineage",
    "protected_json",
    "role_providers_json",
];

const PROJECT_REPO_COLUMNS: &[&str] = &["project", "repo", "scope_json"];

const BACKLOG_COLUMNS: &[&str] = &["id", "project", "text", "created_at", "done_at"];

const INITIATIVE_COLUMNS: &[&str] = &[
    "id",
    "project",
    "outcome",
    "budget_usd",
    "stop_after_same_rule",
    "created_at",
    "settled_at",
];

fn project_from_row(r: &Row) -> rusqlite::Result<Project> {
    let protected_json: Option<String> = r.get("protected_json")?;
    let role_providers_json: Option<String> = r.get("role_providers_json")?;
    Ok(Project {
        name: r.get("name")?,
        purpose: r.get("purpose")?,
        created_at: r.get("created_at")?,
        workflow: r.get("workflow")?,
        per_task_usd: r.get("per_task_usd")?,
        per_initiative_usd: r.get("per_initiative_usd")?,
        supervisor_model: r.get("supervisor_model")?,
        supervisor_per_lineage: r.get("supervisor_per_lineage")?,
        protected: protected_json.map(|j| serde_json::from_str(&j).unwrap_or_default()),
        role_providers: role_providers_json
            .map(|j| serde_json::from_str(&j).unwrap_or_default())
            .unwrap_or_default(),
    })
}

fn project_repo_from_row(r: &Row) -> rusqlite::Result<ProjectRepo> {
    Ok(ProjectRepo {
        repo: r.get("repo")?,
        scope: r.get("scope_json")?,
    })
}

fn backlog_from_row(r: &Row) -> rusqlite::Result<BacklogItem> {
    Ok(BacklogItem {
        id: r.get("id")?,
        project: r.get("project")?,
        text: r.get("text")?,
        created_at: r.get("created_at")?,
        done_at: r.get("done_at")?,
    })
}

fn initiative_from_row(r: &Row) -> rusqlite::Result<Initiative> {
    Ok(Initiative {
        id: r.get("id")?,
        project: r.get("project")?,
        outcome: r.get("outcome")?,
        budget_usd: r.get("budget_usd")?,
        stop_after_same_rule: r.get("stop_after_same_rule")?,
        created_at: r.get("created_at")?,
        settled_at: r.get("settled_at")?,
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
    fn set_project_defaults_purpose_replaces_the_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        s.create_project(&Project {
            name: "p".into(),
            purpose: "Repository /home/x/repo.".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
        assert!(is_placeholder_purpose(
            &s.project("p").unwrap().unwrap().purpose
        ));

        s.set_project_defaults(
            "p",
            &ProjectDefaults {
                purpose: Some("What this project is for.".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        let p = s.project("p").unwrap().unwrap();
        assert_eq!(p.purpose, "What this project is for.");
        assert!(!is_placeholder_purpose(&p.purpose));

        // A `None` purpose (no `--purpose` given) leaves it alone.
        s.set_project_defaults(
            "p",
            &ProjectDefaults {
                workflow: Some("other".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            s.project("p").unwrap().unwrap().purpose,
            "What this project is for."
        );
    }

    #[test]
    fn is_placeholder_purpose_matches_only_the_migrations_shape() {
        assert!(is_placeholder_purpose("Repository /home/x/repo."));
        assert!(is_placeholder_purpose("Repository /home/x/repo"));
        assert!(!is_placeholder_purpose(""));
        assert!(!is_placeholder_purpose("What this project is for."));
        // Starts the same way but is not a path: a real purpose that
        // happens to start with the same word is left alone.
        assert!(!is_placeholder_purpose(
            "Repository of record for this team."
        ));
        assert!(!is_placeholder_purpose("A Repository /home/x."));
    }

    #[test]
    fn ensure_default_project_gives_a_new_project_the_placeholder_purpose() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let repo = dir.path().join("myrepo").display().to_string();
        s.ensure_default_project(&repo).unwrap();
        let p = s.project("myrepo").unwrap().unwrap();
        assert!(is_placeholder_purpose(&p.purpose), "{}", p.purpose);
    }

    #[test]
    fn unknown_state_is_an_error_not_a_default() {
        assert!(TaskState::try_from("bogus").is_err());
        assert!(AttemptState::try_from("bogus").is_err());
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

    /// Fixture: two landed tasks (one landed the ordinary way, one by hand
    /// and later deployed) plus a withdrawn one, all in the same workflow
    /// and project. Exercises both new metrics end to end at the store
    /// level: human attention's four signals (operator answers, hand
    /// landings, withdrawals, hand commits) summed and divided by landed
    /// pieces, and time to live (a deploy's `finished_at` overriding a
    /// task's own `landed_at` once one is tied to it).
    #[test]
    fn human_attention_and_time_to_live_count_hand_landing_withdrawal_and_deploy() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        s.create_project(&Project {
            name: "proj".into(),
            purpose: "p".into(),
            created_at: 0,
            ..Default::default()
        })
        .unwrap();

        let base = |created_at: i64| Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 1,
            timeout_secs: 1,
            state: TaskState::Succeeded,
            created_at,
            started_at: Some(created_at),
            finished_at: Some(created_at + 10),
            workflow: "direct".into(),
            workflow_hash: "h1".into(),
            project: Some("proj".into()),
            ..Default::default()
        };
        let insert = |mut t: Task| {
            t.id = s.insert_task(&t).unwrap();
            s.update_task(&t).unwrap();
            t
        };

        // Task A: landed the ordinary way, no deploy tied to it, and an
        // operator answered a question of its along the way.
        let mut a = base(1000);
        a.landed_sha = "asha".into();
        a.landed_at = Some(1100);
        let a = insert(a);
        s.insert_decision_by(a.id, "r", "q", "operator answered", "operator", "", None)
            .unwrap();

        // Task B: landed by a human's `forge land`, then deployed.
        let mut b = base(2000);
        b.landed_sha = "bsha".into();
        b.landed_at = Some(2500);
        b.hand_landed = true;
        let b = insert(b);
        s.set_hand_commits_cache(b.id, 3, 9999).unwrap();
        let deploy_id = s
            .start_deploy("proj", "prod", "bsha", 2500, Some(b.id))
            .unwrap();
        s.finish_deploy(
            deploy_id, 2600, true, "ok", None, "", None, None, None, None,
        )
        .unwrap();

        // Task C: withdrawn, never landed.
        let mut c = base(3000);
        c.state = TaskState::Withdrawn;
        c.finished_at = Some(3010);
        insert(c);

        let scope = StatsFilter::default();
        let human = s.human_attention_stats(&scope).unwrap();
        assert_eq!(human.len(), 1);
        let h = &human[0];
        assert_eq!(h.landed, 2);
        assert_eq!(h.operator_answers, 1);
        assert_eq!(h.hand_landed, 1);
        assert_eq!(h.withdrawals, 1);
        assert_eq!(
            h.hand_commits, 3,
            "cached per landed task, like repair_cost"
        );

        let human_p = s.human_attention_project_stats().unwrap();
        assert_eq!(human_p.len(), 1);
        let hp = &human_p[0];
        assert_eq!(hp.project, "proj");
        assert_eq!(hp.landed, 2);
        assert_eq!(hp.operator_answers, 1);
        assert_eq!(hp.hand_landed, 1);
        assert_eq!(hp.withdrawals, 1);
        assert_eq!(hp.hand_commits, 3);

        let mut ttls = s.task_ttls(&scope).unwrap();
        ttls.sort_by_key(|t| t.secs);
        assert_eq!(ttls.len(), 2);
        assert_eq!(ttls[0].secs, 100, "task A: landed_at - created_at");
        assert_eq!(
            ttls[1].secs, 600,
            "task B: the tied deploy's finished_at - created_at, not landed_at"
        );
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
    fn a_minted_portal_token_resolves_to_its_project_and_a_stranger_resolves_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        assert!(s.portal_token_project("nope").unwrap().is_none());

        s.create_portal_token("equitizr", "tok1", 1).unwrap();
        assert_eq!(
            s.portal_token_project("tok1").unwrap().as_deref(),
            Some("equitizr")
        );
    }

    #[test]
    fn revoking_a_projects_tokens_stops_them_resolving_but_leaves_other_projects_alone() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        mk_project(&s, "nucleosynthesis");

        s.create_portal_token("equitizr", "tok1", 1).unwrap();
        s.create_portal_token("equitizr", "tok2", 2).unwrap();
        s.create_portal_token("nucleosynthesis", "tok3", 3).unwrap();

        let n = s.revoke_portal_tokens("equitizr", 10).unwrap();
        assert_eq!(n, 2, "both of equitizr's tokens were active");

        assert!(s.portal_token_project("tok1").unwrap().is_none());
        assert!(s.portal_token_project("tok2").unwrap().is_none());
        assert_eq!(
            s.portal_token_project("tok3").unwrap().as_deref(),
            Some("nucleosynthesis"),
            "revoking one project's tokens must not touch another's"
        );

        // Revoking again finds nothing left active.
        assert_eq!(s.revoke_portal_tokens("equitizr", 20).unwrap(), 0);
    }

    #[test]
    fn a_project_can_carry_more_than_one_active_token_until_revoked() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        s.create_portal_token("equitizr", "tok1", 1).unwrap();
        s.create_portal_token("equitizr", "tok2", 2).unwrap();
        assert_eq!(
            s.portal_token_project("tok1").unwrap().as_deref(),
            Some("equitizr"),
            "minting a second token does not itself revoke the first"
        );
        assert_eq!(
            s.portal_token_project("tok2").unwrap().as_deref(),
            Some("equitizr")
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
            Some(true),
            Some("[]"),
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
        assert_eq!(all[0].look_ok, None);
        assert_eq!(all[1].look_ok, Some(true));
        assert_eq!(all[1].look_json.as_deref(), Some("[]"));

        let prod_only = s.deploys("equitizr", Some("prod")).unwrap();
        assert_eq!(prod_only.len(), 1);
        assert_eq!(prod_only[0].id, a);
    }

    fn mk_job(s: &Store, project: &str, workflow: &str, started_at: i64) -> i64 {
        s.create_job(&Job {
            project: project.into(),
            workflow: workflow.into(),
            workflow_hash: "deadbeef".into(),
            landed_sha: "cafef00d".into(),
            trigger_kind: "manual".into(),
            trigger_ref: "".into(),
            state: JobState::Running,
            dry_run: false,
            started_at,
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn a_created_job_is_read_back_with_its_state_round_tripped() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        assert!(s.job(1).unwrap().is_none());

        let id = mk_job(&s, "equitizr", "quote-by-text", 100);
        let j = s.job(id).unwrap().unwrap();
        assert_eq!(j.project, "equitizr");
        assert_eq!(j.workflow, "quote-by-text");
        assert_eq!(j.workflow_hash, "deadbeef");
        assert_eq!(j.landed_sha, "cafef00d");
        assert_eq!(j.trigger_kind, "manual");
        assert_eq!(j.state, JobState::Running);
        assert!(!j.dry_run);
        assert_eq!(j.started_at, 100);
        assert!(j.finished_at.is_none());
        assert!(j.cost_usd.is_none());
    }

    #[test]
    fn finish_job_sets_state_cost_and_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        let id = mk_job(&s, "equitizr", "quote-by-text", 100);

        s.finish_job(
            id,
            130,
            JobState::Ok,
            Some(0.02),
            r#"[{"level":"L0","name":"quoted","ok":true}]"#,
        )
        .unwrap();

        let j = s.job(id).unwrap().unwrap();
        assert_eq!(j.state, JobState::Ok);
        assert_eq!(j.finished_at, Some(130));
        assert_eq!(j.cost_usd, Some(0.02));
        assert!(j.verdict_json.contains("quoted"));
    }

    #[test]
    fn jobs_lists_newest_first_and_filters_by_project_and_state() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        mk_project(&s, "nucleosynthesis");

        let a = mk_job(&s, "equitizr", "quote-by-text", 100);
        let b = mk_job(&s, "equitizr", "quote-by-text", 200);
        let c = mk_job(&s, "nucleosynthesis", "other", 300);
        s.finish_job(b, 250, JobState::Failed, None, "[]").unwrap();

        let all = s.jobs(None, None).unwrap();
        assert_eq!(
            all.iter().map(|j| j.id).collect::<Vec<_>>(),
            vec![c, b, a],
            "newest first"
        );

        let equitizr_only = s.jobs(Some("equitizr"), None).unwrap();
        assert_eq!(
            equitizr_only.iter().map(|j| j.id).collect::<Vec<_>>(),
            vec![b, a]
        );

        let failed_only = s.jobs(None, Some(JobState::Failed)).unwrap();
        assert_eq!(failed_only.len(), 1);
        assert_eq!(failed_only[0].id, b);

        let equitizr_running = s.jobs(Some("equitizr"), Some(JobState::Running)).unwrap();
        assert_eq!(equitizr_running.len(), 1);
        assert_eq!(equitizr_running[0].id, a);
    }

    #[test]
    fn job_steps_and_effects_are_recorded_and_read_back_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        let id = mk_job(&s, "equitizr", "quote-by-text", 100);

        assert!(s.job_steps(id).unwrap().is_empty());
        assert!(s.job_effects(id).unwrap().is_empty());

        s.append_job_step(&JobStep {
            job_id: id,
            seq: 0,
            action: "extract-job".into(),
            kind: "directive".into(),
            provider: "anthropic".into(),
            model: "haiku".into(),
            cost_usd: Some(0.001),
            started_at: 100,
            finished_at: Some(101),
            output_ref: "step-0.json".into(),
            ..Default::default()
        })
        .unwrap();
        s.append_job_step(&JobStep {
            job_id: id,
            seq: 1,
            action: "send-quote".into(),
            kind: "operation".into(),
            started_at: 101,
            finished_at: Some(102),
            exit_code: Some(0),
            output_ref: "step-1.json".into(),
            ..Default::default()
        })
        .unwrap();

        s.append_job_effect(&JobEffect {
            job_id: id,
            seq: 1,
            kind: "message".into(),
            target: "+15555550100".into(),
            summary: "quoted the Hendersons' fence job at $1,240".into(),
            dry_run: false,
            ..Default::default()
        })
        .unwrap();

        let steps = s.job_steps(id).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].action, "extract-job");
        assert_eq!(steps[0].kind, "directive");
        assert_eq!(steps[0].provider, "anthropic");
        assert_eq!(steps[1].action, "send-quote");
        assert_eq!(steps[1].exit_code, Some(0));

        let effects = s.job_effects(id).unwrap();
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].kind, "message");
        assert_eq!(effects[0].target, "+15555550100");
        assert!(!effects[0].dry_run);
    }

    #[test]
    fn job_effects_for_project_spans_every_job_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        mk_project(&s, "nucleosynthesis");
        let a = mk_job(&s, "equitizr", "quote-by-text", 100);
        let b = mk_job(&s, "equitizr", "quote-by-text", 200);
        let c = mk_job(&s, "nucleosynthesis", "other", 300);

        s.append_job_effect(&JobEffect {
            job_id: a,
            seq: 0,
            kind: "message".into(),
            target: "customer-a".into(),
            summary: "first".into(),
            dry_run: false,
            ..Default::default()
        })
        .unwrap();
        s.append_job_effect(&JobEffect {
            job_id: b,
            seq: 0,
            kind: "row".into(),
            target: "book".into(),
            summary: "second".into(),
            dry_run: true,
            ..Default::default()
        })
        .unwrap();
        s.append_job_effect(&JobEffect {
            job_id: c,
            seq: 0,
            kind: "message".into(),
            target: "customer-c".into(),
            summary: "other project".into(),
            dry_run: false,
            ..Default::default()
        })
        .unwrap();

        let effects = s.job_effects_for_project("equitizr").unwrap();
        assert_eq!(effects.len(), 2);
        assert_eq!(effects[0].summary, "second", "newest first");
        assert_eq!(effects[0].job_id, b);
        assert_eq!(effects[1].summary, "first");
        assert_eq!(effects[1].job_id, a);
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
        stmt.query_map([], |r| r.get::<_, String>("name"))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    #[test]
    fn the_column_lists_agree_with_the_schema() {
        let (_d, store) = open();
        for (table, cols) in [
            ("tasks", TASK_COLUMNS),
            ("attempts", ATTEMPT_COLUMNS),
            ("jobs", JOB_COLUMNS),
            ("job_steps", JOB_STEP_COLUMNS),
            ("job_effects", JOB_EFFECT_COLUMNS),
            ("deploys", DEPLOY_COLUMNS),
            ("deploy_targets", DEPLOY_TARGET_COLUMNS),
            ("projects", PROJECT_COLUMNS),
            ("project_repos", PROJECT_REPO_COLUMNS),
            ("backlog", BACKLOG_COLUMNS),
            ("initiatives", INITIATIVE_COLUMNS),
            ("ops", OP_COLUMNS),
            ("decisions", DECISION_COLUMNS),
            ("task_refs", TASK_REF_COLUMNS),
            ("assessments", ASSESSMENT_COLUMNS),
        ] {
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

    /// Every `r.get(` call in the file, with the index that follows it (an
    /// optional `::<Type>` turbofish is skipped first), one entry per call.
    fn positional_row_gets(src: &str) -> Vec<(usize, String, u32)> {
        let mut out = Vec::new();
        for (lineno, line) in src.lines().enumerate() {
            let mut start = 0;
            while let Some(rel) = line[start..].find("r.get") {
                let mut pos = start + rel + "r.get".len();
                if line[pos..].starts_with("::<") {
                    pos += 3;
                    let mut depth = 1;
                    let bytes = line.as_bytes();
                    while depth > 0 && pos < bytes.len() {
                        match bytes[pos] as char {
                            '<' => depth += 1,
                            '>' => depth -= 1,
                            _ => {}
                        }
                        pos += 1;
                    }
                }
                if line[pos..].starts_with('(') {
                    let digits: String = line[pos + 1..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    if let Ok(n) = digits.parse::<u32>() {
                        out.push((lineno + 1, line.trim().to_string(), n));
                    }
                }
                start = pos.max(start + rel + 1);
            }
        }
        out
    }

    /// A `*_from_row` function or a `query_map`/`query_row` closure that
    /// reads `r.get(N)` for `N > 0` has the exact defect item 2 of
    /// docs/REVIEW-2.md describes: inserting a column mid-`SELECT`
    /// mis-parses silently. `r.get(0)` alone is left alone: by the time
    /// this task is done, the only statements still reading it are
    /// single-column queries (a `COUNT(*)`, a bare `id`, or the like)
    /// where there is no second field to drift out of order against.
    #[test]
    fn no_row_reads_a_column_by_position_outside_a_single_column_query() {
        let files: &[(&str, &str)] = &[
            ("mod.rs", include_str!("mod.rs")),
            ("tasks.rs", include_str!("tasks.rs")),
            ("attempts.rs", include_str!("attempts.rs")),
        ];
        let offenders: Vec<String> = files
            .iter()
            .flat_map(|(name, src)| {
                positional_row_gets(src)
                    .into_iter()
                    .filter(|(_, _, n)| *n != 0)
                    .map(move |(lineno, text, _)| format!("{name}:{lineno}: {text}"))
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "r.get(N) for N > 0 reads a column by position; name it instead \
             (see e.g. OP_COLUMNS/op_from_row for the pattern):\n{}",
            offenders.join("\n")
        );
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
