//! One SQLite file, two tables. A task is what the operator asked for; an
//! attempt is one run of the agent against it. The numbers live on attempts
//! and every one of them was computed by Forge, git, or the CLI's accounting.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::path::Path;

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
    pub fn parse(s: &str) -> TaskState {
        match s {
            "running" => TaskState::Running,
            "succeeded" => TaskState::Succeeded,
            "failed" => TaskState::Failed,
            "unverified" => TaskState::Unverified,
            _ => TaskState::Queued,
        }
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
    pub fn parse(s: &str) -> AttemptState {
        match s {
            "succeeded" => AttemptState::Succeeded,
            "checks_failed" => AttemptState::ChecksFailed,
            "agent_failed" => AttemptState::AgentFailed,
            "unverified" => AttemptState::Unverified,
            _ => AttemptState::Running,
        }
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
    pub state: TaskState,
    pub reason: String,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub pushed: bool,
}

#[derive(Default, Debug, Clone)]
pub struct Attempt {
    pub id: i64,
    pub task_id: i64,
    pub attempt_no: i64,
    pub state: AttemptState,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub agent_exit: Option<i32>,
    pub num_turns: i64,
    pub tool_calls: i64,
    pub cost_usd: Option<f64>,
    pub agent_ms: i64,
    pub commits: i64,
    pub files_changed: i64,
    pub dirty: bool,
    pub checks_json: String,
    pub result_text: String,
    pub log_path: String,
}

pub struct Store {
    conn: Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tasks (
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
  state TEXT NOT NULL,
  reason TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  started_at INTEGER,
  finished_at INTEGER,
  pushed INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS attempts (
  id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(id),
  attempt_no INTEGER NOT NULL,
  state TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  agent_exit INTEGER,
  num_turns INTEGER NOT NULL DEFAULT 0,
  tool_calls INTEGER NOT NULL DEFAULT 0,
  cost_usd REAL,
  agent_ms INTEGER NOT NULL DEFAULT 0,
  commits INTEGER NOT NULL DEFAULT 0,
  files_changed INTEGER NOT NULL DEFAULT 0,
  dirty INTEGER NOT NULL DEFAULT 0,
  checks_json TEXT NOT NULL DEFAULT '[]',
  result_text TEXT NOT NULL DEFAULT '',
  log_path TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS attempts_task ON attempts(task_id, attempt_no);
";

const TASK_COLS: &str =
    "id, repo, task, base_branch, base_sha, branch, worktree, model, max_turns, max_attempts,
    state, reason, created_at, started_at, finished_at, pushed";

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
        state: TaskState::parse(&r.get::<_, String>(10)?),
        reason: r.get(11)?,
        created_at: r.get(12)?,
        started_at: r.get(13)?,
        finished_at: r.get(14)?,
        pushed: r.get::<_, i64>(15)? != 0,
    })
}

const ATTEMPT_COLS: &str =
    "id, task_id, attempt_no, state, started_at, finished_at, agent_exit, num_turns, tool_calls,
    cost_usd, agent_ms, commits, files_changed, dirty, checks_json, result_text, log_path";

fn attempt_from_row(r: &Row) -> rusqlite::Result<Attempt> {
    Ok(Attempt {
        id: r.get(0)?,
        task_id: r.get(1)?,
        attempt_no: r.get(2)?,
        state: AttemptState::parse(&r.get::<_, String>(3)?),
        started_at: r.get(4)?,
        finished_at: r.get(5)?,
        agent_exit: r.get(6)?,
        num_turns: r.get(7)?,
        tool_calls: r.get(8)?,
        cost_usd: r.get(9)?,
        agent_ms: r.get(10)?,
        commits: r.get(11)?,
        files_changed: r.get(12)?,
        dirty: r.get::<_, i64>(13)? != 0,
        checks_json: r.get(14)?,
        result_text: r.get(15)?,
        log_path: r.get(16)?,
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    pub fn insert_task(&self, t: &Task) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO tasks (repo, task, base_branch, model, max_turns, max_attempts, state, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![t.repo, t.task, t.base_branch, t.model, t.max_turns, t.max_attempts, t.state.as_str(), t.created_at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_task(&self, t: &Task) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET base_sha=?2, branch=?3, worktree=?4, state=?5, reason=?6, started_at=?7, finished_at=?8, pushed=?9
             WHERE id=?1",
            params![
                t.id,
                t.base_sha,
                t.branch,
                t.worktree,
                t.state.as_str(),
                t.reason,
                t.started_at,
                t.finished_at,
                t.pushed as i64
            ],
        )?;
        Ok(())
    }

    pub fn task(&self, id: i64) -> Result<Option<Task>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {TASK_COLS} FROM tasks WHERE id=?1"),
                params![id],
                task_from_row,
            )
            .optional()?)
    }

    pub fn insert_attempt(&self, a: &Attempt) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO attempts (task_id, attempt_no, state, started_at, log_path) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![a.task_id, a.attempt_no, a.state.as_str(), a.started_at, a.log_path],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_attempt(&self, a: &Attempt) -> Result<()> {
        self.conn.execute(
            "UPDATE attempts SET state=?2, finished_at=?3, agent_exit=?4, num_turns=?5, tool_calls=?6, cost_usd=?7,
             agent_ms=?8, commits=?9, files_changed=?10, dirty=?11, checks_json=?12, result_text=?13
             WHERE id=?1",
            params![
                a.id,
                a.state.as_str(),
                a.finished_at,
                a.agent_exit,
                a.num_turns,
                a.tool_calls,
                a.cost_usd,
                a.agent_ms,
                a.commits,
                a.files_changed,
                a.dirty as i64,
                a.checks_json,
                a.result_text
            ],
        )?;
        Ok(())
    }

    pub fn attempts(&self, task_id: i64) -> Result<Vec<Attempt>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ATTEMPT_COLS} FROM attempts WHERE task_id=?1 ORDER BY attempt_no"
        ))?;
        let rows = stmt.query_map(params![task_id], attempt_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Total cost of a task's attempts so far, from the CLI's accounting.
    pub fn task_cost(&self, task_id: i64) -> Result<f64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0) FROM attempts WHERE task_id=?1",
            params![task_id],
            |r| r.get(0),
        )?)
    }

    pub fn print_log(&self, limit: u32) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.state, datetime(t.created_at,'unixepoch','localtime'), t.repo, t.task,
                    (SELECT COUNT(*) FROM attempts a WHERE a.task_id=t.id),
                    (SELECT COALESCE(SUM(cost_usd),0) FROM attempts a WHERE a.task_id=t.id)
             FROM tasks t ORDER BY t.id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, f64>(6)?,
            ))
        })?;
        println!(
            "{:<5} {:<11} {:<3} {:<8} {:<19} {:<18} TASK",
            "ID", "STATE", "ATT", "COST", "CREATED", "REPO"
        );
        for row in rows {
            let (id, state, created, repo, task, attempts, cost) = row?;
            let repo_name = Path::new(&repo)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or(repo);
            let task_short: String = task.chars().take(50).collect::<String>().replace('\n', " ");
            println!(
                "{:<5} {:<11} {:<3} {:<8} {:<19} {:<18} {}",
                id,
                state,
                attempts,
                format!("${cost:.4}"),
                created,
                repo_name,
                task_short
            );
        }
        Ok(())
    }

    pub fn print_show(&self, id: i64) -> Result<()> {
        let Some(t) = self.task(id)? else {
            bail!("no task {id}")
        };
        let attempts = self.attempts(id)?;
        let cost: f64 = attempts.iter().filter_map(|a| a.cost_usd).sum();
        println!("task       {}", t.id);
        println!(
            "state      {}{}",
            t.state.as_str(),
            if t.reason.is_empty() {
                String::new()
            } else {
                format!(" ({})", t.reason)
            }
        );
        println!("repo       {}", t.repo);
        println!(
            "base       {} @ {}",
            t.base_branch,
            if t.base_sha.is_empty() {
                "-"
            } else {
                &t.base_sha[..8]
            }
        );
        println!(
            "branch     {}{}",
            if t.branch.is_empty() { "-" } else { &t.branch },
            if t.pushed { " (pushed)" } else { "" }
        );
        println!(
            "worktree   {}",
            if t.worktree.is_empty() {
                "-"
            } else {
                &t.worktree
            }
        );
        println!(
            "model      {} (max {} turns, max {} attempts)",
            t.model, t.max_turns, t.max_attempts
        );
        println!("cost       ${cost:.4} over {} attempt(s)", attempts.len());
        println!("text       {}", t.task);
        for a in &attempts {
            println!();
            println!(
                "attempt {}  {}  exit {}  {} turns  {} tools  {:.1}s  {}  {} commit(s)  {} file(s){}",
                a.attempt_no,
                a.state.as_str(),
                a.agent_exit.map_or("-".into(), |v| v.to_string()),
                a.num_turns,
                a.tool_calls,
                a.agent_ms as f64 / 1000.0,
                a.cost_usd.map_or("-".into(), |c| format!("${c:.4}")),
                a.commits,
                a.files_changed,
                if a.dirty { "  DIRTY" } else { "" }
            );
            println!("  log     {}", a.log_path);
            println!("  checks  {}", a.checks_json);
            if !a.result_text.is_empty() {
                let first: String = a
                    .result_text
                    .lines()
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" / ");
                println!("  result  {}", first.chars().take(200).collect::<String>());
            }
        }
        Ok(())
    }
}
