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
mod deploys;
mod jobs;
mod projects;
mod record;
mod stats;
mod tasks;

pub use attempts::{Attempt, AttemptState, FinishAttempt, Op};
pub use deploys::{Assessment, Deploy, DeployTarget};
pub use jobs::{Job, JobEffect, JobStat, JobState, JobStep};
pub use projects::{
    BacklogItem, Initiative, InitiativeUpdate, Project, ProjectDefaults, ProjectRepo, ProjectStat,
    ProjectTaskStats, is_placeholder_purpose,
};
pub use record::{Decision, TaskRef};
pub use stats::{
    HumanAttentionProjectStat, HumanAttentionStat, JournalStat, RoleStat, StatsFilter, StepStat,
    TaskTtl, WorkflowStat,
};
pub use tasks::{Task, TaskState};

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
    // A schedule trigger's slot (docs/JOBS.md, "Triggers"): one job per
    // project, workflow and slot, so a tick that reconsiders an already-
    // started slot (a second worker pass before the next one comes due, a
    // restart replaying the same tick) fails the insert instead of
    // starting a second job for it. Manual and other triggers share the
    // same `trigger_ref` column but are never unique on it, so the index
    // is partial.
    "
CREATE UNIQUE INDEX jobs_schedule_slot ON jobs(project, workflow, trigger_ref) WHERE trigger_kind = 'schedule';
",
    // A delayed job (docs/JOBS.md, "Delayed jobs"): `due_at` is the unix
    // second `claim_next_job` compares against now before a `scheduled`
    // job is claimable. The wait is this column, never an in-memory timer,
    // so it survives a worker restart. NULL for every job that was never
    // delayed, including every one recorded before this column existed.
    "
ALTER TABLE jobs ADD COLUMN due_at INTEGER;
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
    fn unknown_state_is_an_error_not_a_default() {
        assert!(TaskState::try_from("bogus").is_err());
        assert!(AttemptState::try_from("bogus").is_err());
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
            ("attempts", attempts::ATTEMPT_COLUMNS),
            ("jobs", jobs::JOB_COLUMNS),
            ("job_steps", jobs::JOB_STEP_COLUMNS),
            ("job_effects", jobs::JOB_EFFECT_COLUMNS),
            ("deploys", deploys::DEPLOY_COLUMNS),
            ("deploy_targets", deploys::DEPLOY_TARGET_COLUMNS),
            ("projects", projects::PROJECT_COLUMNS),
            ("project_repos", projects::PROJECT_REPO_COLUMNS),
            ("backlog", projects::BACKLOG_COLUMNS),
            ("initiatives", projects::INITIATIVE_COLUMNS),
            ("ops", attempts::OP_COLUMNS),
            ("decisions", record::DECISION_COLUMNS),
            ("task_refs", record::TASK_REF_COLUMNS),
            ("assessments", deploys::ASSESSMENT_COLUMNS),
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
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/store");
        let files: Vec<(String, String)> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "rs"))
            .map(|p| {
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                let src = std::fs::read_to_string(&p).unwrap();
                (name, src)
            })
            .collect();
        assert!(
            !files.is_empty(),
            "no .rs files found under {}",
            dir.display()
        );
        assert!(
            files.iter().any(|(name, _)| name == "mod.rs"),
            "expected mod.rs among files under {}",
            dir.display()
        );
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

    /// One file per table family (docs/REVIEW-2.md stage 2): no file under
    /// src/store/ should grow back into the single unnavigable store.rs the
    /// review found. 1500 lines is a bound with room, not a target.
    #[test]
    fn no_store_file_is_over_1500_lines() {
        let files: &[(&str, &str)] = &[
            ("mod.rs", include_str!("mod.rs")),
            ("tasks.rs", include_str!("tasks.rs")),
            ("attempts.rs", include_str!("attempts.rs")),
            ("jobs.rs", include_str!("jobs.rs")),
            ("deploys.rs", include_str!("deploys.rs")),
            ("projects.rs", include_str!("projects.rs")),
            ("record.rs", include_str!("record.rs")),
            ("stats.rs", include_str!("stats.rs")),
        ];
        for (name, src) in files {
            let lines = src.lines().count();
            assert!(
                lines <= 1500,
                "{name} is {lines} lines, over the 1500-line bound"
            );
        }
    }
}
