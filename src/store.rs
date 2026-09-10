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
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Queued => "queued",
            TaskState::Running => "running",
            TaskState::Succeeded => "succeeded",
            TaskState::Failed => "failed",
            TaskState::Unverified => "unverified",
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
}

impl AttemptState {
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptState::Running => "running",
            AttemptState::Succeeded => "succeeded",
            AttemptState::ChecksFailed => "checks_failed",
            AttemptState::AgentFailed => "agent_failed",
            AttemptState::Unverified => "unverified",
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
}

#[derive(Default, Debug, Clone)]
pub struct Attempt {
    pub id: i64,
    pub task_id: i64,
    pub attempt_no: i64,
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
}

pub struct TaskSummary {
    pub id: i64,
    pub state: String,
    pub created: String,
    pub repo: String,
    pub task: String,
    pub attempts: i64,
    pub cost: f64,
}

pub struct Store {
    conn: Mutex<Connection>,
}

/// Forward-only. Index = version - 1. Never edit a shipped entry; append.
const MIGRATIONS: &[&str] = &["
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
"];

const TASK_COLS: &str = "id, repo, task, base_branch, base_sha, branch, worktree, model, max_turns, max_attempts,
    timeout_secs, checks_json, state, reason, created_at, started_at, finished_at, pushed, worker_pid, budget_usd,
    worktree_removed_at";

fn conv<T, E: std::error::Error + Send + Sync + 'static>(
    idx: usize,
    r: std::result::Result<T, E>,
) -> rusqlite::Result<T> {
    r.map_err(|e| rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e)))
}

fn task_from_row(r: &Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get(0)?,
        repo: r.get(1)?,
        task: r.get(2)?,
        base_branch: r.get(3)?,
        base_sha: r.get(4)?,
        branch: r.get(5)?,
        worktree: r.get(6)?,
        model: r.get(7)?,
        max_turns: r.get(8)?,
        max_attempts: r.get(9)?,
        timeout_secs: r.get(10)?,
        checks: conv(11, serde_json::from_str(&r.get::<_, String>(11)?))?,
        state: conv(12, TaskState::try_from(r.get::<_, String>(12)?.as_str()))?,
        reason: r.get(13)?,
        created_at: r.get(14)?,
        started_at: r.get(15)?,
        finished_at: r.get(16)?,
        pushed: r.get::<_, i64>(17)? != 0,
        worker_pid: r.get(18)?,
        budget_usd: r.get(19)?,
        worktree_removed_at: r.get(20)?,
    })
}

const ATTEMPT_COLS: &str = "id, task_id, attempt_no, state, reason, started_at, finished_at, agent_exit, timed_out,
    num_turns, tool_calls, cost_usd, agent_ms, commits, files_changed, dirty, verdict_json, result_text, log_path";

fn attempt_from_row(r: &Row) -> rusqlite::Result<Attempt> {
    Ok(Attempt {
        id: r.get(0)?,
        task_id: r.get(1)?,
        attempt_no: r.get(2)?,
        state: conv(3, AttemptState::try_from(r.get::<_, String>(3)?.as_str()))?,
        reason: r.get(4)?,
        started_at: r.get(5)?,
        finished_at: r.get(6)?,
        agent_exit: r.get(7)?,
        timed_out: r.get::<_, i64>(8)? != 0,
        num_turns: r.get(9)?,
        tool_calls: r.get(10)?,
        cost_usd: r.get(11)?,
        agent_ms: r.get(12)?,
        commits: r.get(13)?,
        files_changed: r.get(14)?,
        dirty: r.get::<_, i64>(15)? != 0,
        verdict_json: r.get(16)?,
        result_text: r.get(17)?,
        log_path: r.get(18)?,
    })
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

    #[cfg(test)]
    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .lock()
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    pub fn insert_task(&self, t: &Task) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO tasks (repo, task, base_branch, model, max_turns, max_attempts, timeout_secs, checks_json,
                                state, created_at, budget_usd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                t.budget_usd
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn update_task(&self, t: &Task) -> Result<()> {
        self.lock().execute(
            "UPDATE tasks SET base_sha=?2, branch=?3, worktree=?4, state=?5, reason=?6, started_at=?7, finished_at=?8,
             pushed=?9, worker_pid=?10 WHERE id=?1",
            params![
                t.id,
                t.base_sha,
                t.branch,
                t.worktree,
                t.state.as_str(),
                t.reason,
                t.started_at,
                t.finished_at,
                t.pushed as i64,
                t.worker_pid
            ],
        )?;
        Ok(())
    }

    pub fn task(&self, id: i64) -> Result<Option<Task>> {
        Ok(self
            .lock()
            .query_row(
                &format!("SELECT {TASK_COLS} FROM tasks WHERE id=?1"),
                params![id],
                task_from_row,
            )
            .optional()?)
    }

    /// Atomically take the oldest queued task for this worker.
    pub fn claim_next(&self, pid: i64) -> Result<Option<Task>> {
        let id: Option<i64> = self
            .lock()
            .query_row(
                "UPDATE tasks SET state='running', worker_pid=?1, started_at=?2
                 WHERE id = (SELECT id FROM tasks WHERE state='queued' ORDER BY id LIMIT 1)
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
            "INSERT INTO attempts (task_id, attempt_no, state, started_at, log_path) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![a.task_id, a.attempt_no, a.state.as_str(), a.started_at, a.log_path],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn finish_attempt(&self, a: &Attempt) -> Result<()> {
        self.lock().execute(
            "UPDATE attempts SET state=?2, reason=?3, finished_at=?4, agent_exit=?5, timed_out=?6, num_turns=?7,
             tool_calls=?8, cost_usd=?9, agent_ms=?10, commits=?11, files_changed=?12, dirty=?13, verdict_json=?14,
             result_text=?15 WHERE id=?1",
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
                a.result_text
            ],
        )?;
        Ok(())
    }

    pub fn attempts(&self, task_id: i64) -> Result<Vec<Attempt>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {ATTEMPT_COLS} FROM attempts WHERE task_id=?1 ORDER BY attempt_no"
        ))?;
        let rows = stmt.query_map(params![task_id], attempt_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
            "SELECT {TASK_COLS} FROM tasks WHERE worktree != '' AND worktree_removed_at IS NULL ORDER BY id"
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

    pub fn list_tasks(&self, limit: u32) -> Result<Vec<TaskSummary>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.id, t.state, datetime(t.created_at,'unixepoch','localtime'), t.repo, t.task,
                    (SELECT COUNT(*) FROM attempts a WHERE a.task_id=t.id),
                    (SELECT COALESCE(SUM(cost_usd),0) FROM attempts a WHERE a.task_id=t.id)
             FROM tasks t ORDER BY t.id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(TaskSummary {
                id: r.get(0)?,
                state: r.get(1)?,
                created: r.get(2)?,
                repo: r.get(3)?,
                task: r.get(4)?,
                attempts: r.get(5)?,
                cost: r.get(6)?,
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
}
