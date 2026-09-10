//! One SQLite file, one table. The facts table is the product.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum State {
    #[default]
    Running,
    Succeeded,
    ChecksFailed,
    AgentFailed,
    Unverified,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Succeeded => "succeeded",
            State::ChecksFailed => "checks_failed",
            State::AgentFailed => "agent_failed",
            State::Unverified => "unverified",
        }
    }

    pub fn parse(s: &str) -> State {
        match s {
            "succeeded" => State::Succeeded,
            "checks_failed" => State::ChecksFailed,
            "agent_failed" => State::AgentFailed,
            "unverified" => State::Unverified,
            _ => State::Running,
        }
    }
}

#[derive(Default, Debug)]
pub struct Attempt {
    pub id: String,
    pub repo: String,
    pub task: String,
    pub branch: String,
    pub worktree: String,
    pub base_sha: String,
    pub model: String,
    pub state: State,
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
}

pub struct Store {
    conn: Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS attempts (
  id TEXT PRIMARY KEY,
  repo TEXT NOT NULL,
  task TEXT NOT NULL,
  branch TEXT NOT NULL,
  worktree TEXT NOT NULL,
  base_sha TEXT NOT NULL,
  model TEXT NOT NULL,
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
  result_text TEXT NOT NULL DEFAULT ''
);
";

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    pub fn insert(&self, a: &Attempt) -> Result<()> {
        self.conn.execute(
            "INSERT INTO attempts (id, repo, task, branch, worktree, base_sha, model, state, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![a.id, a.repo, a.task, a.branch, a.worktree, a.base_sha, a.model, a.state.as_str(), a.started_at],
        )?;
        Ok(())
    }

    pub fn finish(&self, a: &Attempt) -> Result<()> {
        self.conn.execute(
            "UPDATE attempts SET state=?2, finished_at=?3, agent_exit=?4, num_turns=?5, tool_calls=?6,
             cost_usd=?7, agent_ms=?8, commits=?9, files_changed=?10, dirty=?11, checks_json=?12, result_text=?13
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

    pub fn print_log(&self, limit: u32) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "SELECT id, state, datetime(started_at,'unixepoch','localtime'), repo, num_turns, cost_usd, commits, task
             FROM attempts ORDER BY started_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<f64>>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, String>(7)?,
            ))
        })?;
        println!(
            "{:<16} {:<14} {:<19} {:<5} {:<8} {:<3} {:<20} TASK",
            "ID", "STATE", "STARTED", "TURNS", "COST", "CMT", "REPO"
        );
        for row in rows {
            let (id, state, started, repo, turns, cost, commits, task) = row?;
            let repo_name = Path::new(&repo)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or(repo);
            let task_short: String = task.chars().take(50).collect::<String>().replace('\n', " ");
            println!(
                "{:<16} {:<14} {:<19} {:<5} {:<8} {:<3} {:<20} {}",
                id,
                state,
                started,
                turns,
                cost.map_or("-".to_string(), |c| format!("${c:.4}")),
                commits,
                repo_name,
                task_short
            );
        }
        Ok(())
    }

    pub fn print_show(&self, id: &str) -> Result<()> {
        let a: Option<Attempt> = self
            .conn
            .query_row(
                "SELECT id, repo, task, branch, worktree, base_sha, model, state, started_at, finished_at, agent_exit,
                        num_turns, tool_calls, cost_usd, agent_ms, commits, files_changed, dirty, checks_json, result_text
                 FROM attempts WHERE id=?1",
                params![id],
                |r| {
                    Ok(Attempt {
                        id: r.get(0)?,
                        repo: r.get(1)?,
                        task: r.get(2)?,
                        branch: r.get(3)?,
                        worktree: r.get(4)?,
                        base_sha: r.get(5)?,
                        model: r.get(6)?,
                        state: State::parse(&r.get::<_, String>(7)?),
                        started_at: r.get(8)?,
                        finished_at: r.get(9)?,
                        agent_exit: r.get(10)?,
                        num_turns: r.get(11)?,
                        tool_calls: r.get(12)?,
                        cost_usd: r.get(13)?,
                        agent_ms: r.get(14)?,
                        commits: r.get(15)?,
                        files_changed: r.get(16)?,
                        dirty: r.get::<_, i64>(17)? != 0,
                        checks_json: r.get(18)?,
                        result_text: r.get(19)?,
                    })
                },
            )
            .optional()?;
        let Some(a) = a else { bail!("no attempt {id}") };
        println!("id            {}", a.id);
        println!("state         {}", a.state.as_str());
        println!("repo          {}", a.repo);
        println!("branch        {}", a.branch);
        println!("worktree      {}", a.worktree);
        println!("base          {}", a.base_sha);
        println!("model         {}", a.model);
        println!("started       {}", a.started_at);
        println!(
            "finished      {}",
            a.finished_at.map_or("-".into(), |v| v.to_string())
        );
        println!(
            "agent exit    {}",
            a.agent_exit.map_or("-".into(), |v| v.to_string())
        );
        println!("turns/tools   {} / {}", a.num_turns, a.tool_calls);
        println!(
            "cost          {}",
            a.cost_usd.map_or("-".into(), |c| format!("${c:.4}"))
        );
        println!("agent wall    {:.1}s", a.agent_ms as f64 / 1000.0);
        println!(
            "commits/files {} / {}{}",
            a.commits,
            a.files_changed,
            if a.dirty { " (dirty)" } else { "" }
        );
        println!("task          {}", a.task);
        println!("checks        {}", a.checks_json);
        if !a.result_text.is_empty() {
            println!("--- agent result ---\n{}", a.result_text);
        }
        Ok(())
    }
}
