//! One SQLite file, two tables. A task is what the operator asked for; an
//! attempt is one run of the agent against it. The numbers live on attempts
//! and every one of them was computed by Forge, git, or the CLI's
//! accounting. Migrations are forward-only and numbered by `user_version`.

use anyhow::{Context, Result, bail};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};
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
    pub max_turns: i64,
    pub max_attempts: i64,
    pub timeout_secs: i64,
    /// Operator-declared acceptance commands, run as L2 after the repo's checks.
    pub checks: Vec<String>,
    pub state: TaskState,
    pub reason: String,
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
    /// "operator" or "supervisor".
    pub answered_by: String,
    /// What the answer cited, comma-separated: paths, "task N", "decision N".
    pub citations: String,
    /// The task the answer re-queued, when known: its state is the
    /// answer's outcome.
    pub retry_id: Option<i64>,
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
];

const TASK_COLUMNS: &[&str] = &[
    "id",
    "repo",
    "task",
    "base_branch",
    "base_sha",
    "branch",
    "worktree",
    "model",
    "max_turns",
    "max_attempts",
    "timeout_secs",
    "checks_json",
    "state",
    "reason",
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
        context: r.get("context")?,
        context_enabled: r.get::<_, i64>("context_enabled")? != 0,
        resume_on_failure: r.get::<_, i64>("resume_on_failure")? != 0,
        plan: r.get("plan")?,
        landed_sha: r.get("landed_sha")?,
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
            "INSERT INTO tasks (repo, task, base_branch, model, max_turns, max_attempts, timeout_secs, checks_json,
                                state, created_at, budget_usd, allow_protected, workflow, show_checks, workflow_hash, workflow_text, land, after_json, retry_of, journal, context_enabled, resume_on_failure)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            params![
                t.repo,
                t.task,
                t.base_branch,
                t.model,
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
                t.resume_on_failure as i64
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
             context_enabled=?33, resume_on_failure=?34, plan=?35, landed_sha=?36 WHERE id=?1",
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
                t.landed_sha
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

    /// Atomically take the oldest queued task for this worker.
    /// The oldest queued task whose dependencies have all landed (or
    /// succeeded without landing, when they were told not to).
    pub fn claim_next(&self, pid: i64) -> Result<Option<Task>> {
        let id: Option<i64> = self
            .lock()
            .query_row(
                "UPDATE tasks SET state='running', worker_pid=?1, started_at=?2
                 WHERE id = (
                   SELECT t.id FROM tasks t WHERE t.state='queued' AND NOT EXISTS (
                     SELECT 1 FROM json_each(t.after_json) j LEFT JOIN tasks d ON d.id = j.value
                     WHERE d.id IS NULL OR d.state != 'succeeded' OR (d.land = 1 AND d.landed_sha = '')
                   ) ORDER BY t.id LIMIT 1)
                 RETURNING id",
                params![pid, crate::unix_now()],
                |r| r.get(0),
            )
            .optional()?;
        match id {
            Some(id) => self.task(id),
            None => Ok(None),
        }
    }

    /// Atomically take one specific queued task.
    pub fn claim(&self, id: i64, pid: i64) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE tasks SET state='running', worker_pid=?2, started_at=?3 WHERE id=?1 AND state='queued'",
            params![id, pid, crate::unix_now()],
        )?;
        Ok(n == 1)
    }

    /// Block every queued task that waits on a task which ended without
    /// landing. Returns the (dependent, dependency) pairs it blocked.
    pub fn block_dependents(&self) -> Result<Vec<(i64, i64, String)>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.id, d.id, d.state, d.reason FROM tasks t, json_each(t.after_json) j JOIN tasks d ON d.id = j.value
             WHERE t.state='queued' AND d.state IN ('failed', 'unverified')
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
            "INSERT INTO attempts (task_id, attempt_no, state, started_at, log_path, step, start_sha, inputs_json, step_seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![a.task_id, a.attempt_no, a.state.as_str(), a.started_at, a.log_path, a.step, a.start_sha, a.inputs_json, a.step_seq],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn finish_attempt(&self, a: &FinishAttempt) -> Result<()> {
        self.lock().execute(
            "UPDATE attempts SET state=?2, reason=?3, finished_at=?4, agent_exit=?5, timed_out=?6, num_turns=?7,
             tool_calls=?8, cost_usd=?9, agent_ms=?10, commits=?11, files_changed=?12, dirty=?13, verdict_json=?14,
             result_text=?15, envelope_json=?16, rl_five_hour=?17, rl_seven_day=?18, rl_five_hour_resets=?19,
             rl_seven_day_resets=?20, end_sha=?21, outputs_json=?22, session_id=?23, first_edit=?24,
             input_tokens=?25, output_tokens=?26, cache_read_input_tokens=?27, cache_creation_input_tokens=?28 WHERE id=?1",
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
                a.cache_creation_input_tokens

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

    /// The most recent rate-limit sample any attempt recorded.
    pub fn latest_rate_limit(&self) -> Result<Option<RateLimitSample>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT COALESCE(finished_at, started_at), rl_five_hour, rl_seven_day, rl_five_hour_resets, rl_seven_day_resets FROM attempts
                 WHERE rl_five_hour IS NOT NULL OR rl_seven_day IS NOT NULL ORDER BY id DESC LIMIT 1",
                [],
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
    pub fn workflow_stats(&self) -> Result<Vec<WorkflowStat>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.workflow, t.workflow_hash, COUNT(*),
                    SUM(t.state='succeeded'), SUM(t.state='failed'), SUM(t.state='blocked'), SUM(t.state='unverified'),
                    COALESCE((SELECT SUM(a.cost_usd) FROM attempts a WHERE a.task_id IN (SELECT id FROM tasks t2 WHERE t2.workflow=t.workflow AND t2.workflow_hash=t.workflow_hash)), 0),
                    COALESCE((SELECT COUNT(*) FROM attempts a WHERE a.task_id IN (SELECT id FROM tasks t2 WHERE t2.workflow=t.workflow AND t2.workflow_hash=t.workflow_hash)), 0),
                    SUM(t.landed_sha != '')
             FROM tasks t WHERE t.state IN ('succeeded','failed','blocked','unverified') AND t.started_at IS NOT NULL
             GROUP BY t.workflow, t.workflow_hash ORDER BY t.workflow, t.workflow_hash",
        )?;
        let rows = stmt.query_map([], |r| {
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
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Outcomes per workflow step.
    pub fn step_stats(&self) -> Result<Vec<StepStat>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.workflow, a.step, COUNT(*), SUM(a.state='succeeded'), SUM(a.state='agent_failed'),
                    SUM(a.state='checks_failed'), SUM(a.state='needs_input'), AVG(a.num_turns), COALESCE(SUM(a.cost_usd),0), AVG(a.agent_ms),
                    AVG(a.first_edit), AVG(a.input_tokens)
             FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.state != 'running'
             GROUP BY t.workflow, a.step ORDER BY t.workflow, a.step",
        )?;
        let rows = stmt.query_map([], |r| {
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

    /// The listing behind `forge log`: newest first, filtered, and paged by
    /// `before` (ids strictly below it) so a client can scroll back.
    pub fn list_tasks_where(&self, q: &TaskFilter) -> Result<Vec<TaskSummary>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.id, t.state, datetime(t.created_at,'unixepoch','localtime'), t.repo, t.task,
                    (SELECT COUNT(*) FROM attempts a WHERE a.task_id=t.id),
                    (SELECT COALESCE(SUM(cost_usd),0) FROM attempts a WHERE a.task_id=t.id),
                    t.workflow, t.created_at, t.finished_at
             FROM tasks t WHERE (?2 IS NULL OR t.state = ?2) AND (?3 IS NULL OR t.repo = ?3)
               AND (?4 IS NULL OR t.id < ?4)
               AND (?5 IS NULL OR t.task LIKE '%' || ?5 || '%' OR CAST(t.id AS TEXT) = ?5)
               AND (?6 IS NULL OR t.workflow = ?6)
             ORDER BY t.id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(
            params![
                q.limit,
                q.state.map(TaskState::as_str),
                q.repo.as_deref(),
                q.before,
                q.grep.as_deref(),
                q.workflow.as_deref()
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
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Record the operator's answer to a blocked task's question.
    pub fn insert_decision_by(
        &self,
        task_id: i64,
        repo: &str,
        question: &str,
        answer: &str,
        answered_by: &str,
        citations: &str,
    ) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO decisions (task_id, repo, question, answer, created_at, answered_by, citations) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![task_id, repo, question, answer, crate::unix_now(), answered_by, citations],
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
            "SELECT d.id, d.task_id, d.repo, d.question, d.answer, d.created_at, d.answered_by, d.citations, d.retry_id
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

    /// Recorded answers, newest first; narrowed to one repository when given.
    pub fn decisions(&self, repo: Option<&str>) -> Result<Vec<Decision>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT id, task_id, repo, question, answer, created_at, answered_by, citations, retry_id FROM decisions
             WHERE ?1 IS NULL OR repo = ?1 ORDER BY id DESC",
        )?;
        let rows = stmt.query_map(params![repo], |r| {
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
        let r = conn
            .execute_batch(sql)
            .and_then(|_| conn.execute_batch(&format!("PRAGMA user_version={v}")));
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
        assert_eq!(s.claim_next(3).unwrap().map(|t| t.id), Some(id));
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
        s.insert_decision_by(a, "r", "q", "a", "supervisor", "")
            .unwrap();
        assert_eq!(s.supervisor_answers_in_lineage(b).unwrap(), 1);
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
        store.update_task(&t).unwrap();
        let back = store.task(t.id).unwrap().unwrap();
        assert_eq!(format!("{back:?}"), format!("{t:?}"));
    }
}
