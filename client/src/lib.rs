//! `forge-client`: the one Rust client of the `forge` CLI. It runs a
//! `forge` verb, parses its `--json` output, and hands back typed rows —
//! never the database, never the kernel. `docs/CLIENT.md` is the contract
//! this crate implements; every shape below is documented there.
//!
//! Every field on every document is `#[serde(default)]`, so a `forge`
//! binary that hasn't grown a newer field yet still parses: a missing key
//! becomes the type's default, never an error.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::io::BufRead;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

/// The `forge` binary: `FORGE_BIN`, else `forge` on `PATH` — resolved
/// exactly as `tui/src/main.rs` resolves it.
pub struct Forge {
    pub bin: String,
}

impl Default for Forge {
    fn default() -> Self {
        Self::new()
    }
}

impl Forge {
    pub fn new() -> Forge {
        Forge {
            bin: std::env::var("FORGE_BIN").unwrap_or_else(|_| "forge".into()),
        }
    }

    /// Runs `forge <args>`, returning stdout as text. A non-zero exit is
    /// an error carrying stderr, per `docs/CLIENT.md`: a client shows the
    /// error and never parses stdout in that case.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        let out = Command::new(&self.bin)
            .args(args)
            .output()
            .with_context(|| format!("running {} {}", self.bin, args.join(" ")))?;
        if !out.status.success() {
            anyhow::bail!(
                "forge {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Runs `forge <args>` and parses stdout as one JSON value.
    pub fn json(&self, args: &[&str]) -> Result<Value> {
        let text = self.run(args)?;
        serde_json::from_str(&text).with_context(|| format!("parsing forge {}", args.join(" ")))
    }

    /// `forge snapshot`: the whole state at one instant, plus the point in
    /// the event log to subscribe from.
    pub fn snapshot(&self) -> Result<Snapshot> {
        let v = self.json(&["snapshot"])?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge events --since <offset> --follow`, as an iterator of typed
    /// events. The subordinate process is killed when the iterator is
    /// dropped.
    pub fn subscribe(&self, offset: u64) -> Result<Subscription> {
        let mut child = Command::new(&self.bin)
            .args(["events", "--since", &offset.to_string(), "--follow"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("running {} events --follow", self.bin))?;
        let stdout = child.stdout.take().context("events stdout")?;
        Ok(Subscription {
            child: Arc::new(Mutex::new(child)),
            lines: std::io::BufReader::new(stdout).lines(),
        })
    }
}

/// The live iterator behind [`Forge::subscribe`]. Yields one [`Event`] per
/// line of `forge events --follow`; lines that aren't valid JSON are
/// skipped. Killing its subordinate `forge events` process on drop is what
/// makes it safe to stop iterating early.
pub struct Subscription {
    child: Arc<Mutex<Child>>,
    lines: std::io::Lines<std::io::BufReader<ChildStdout>>,
}

impl Subscription {
    /// A cloneable handle that kills the subordinate process from any
    /// thread — including one that is, right now, blocked in `next()` on
    /// another thread: killing the process closes its stdout, which wakes
    /// that read with EOF. This is what lets a caller that reads a
    /// `Subscription` on a background thread still tear it down promptly
    /// from its main thread.
    pub fn killer(&self) -> Killer {
        Killer(Arc::clone(&self.child))
    }
}

impl Iterator for Subscription {
    type Item = Event;

    fn next(&mut self) -> Option<Event> {
        loop {
            let line = self.lines.next()?.ok()?;
            if let Ok(event) = serde_json::from_str(&line) {
                return Some(event);
            }
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

/// See [`Subscription::killer`].
#[derive(Clone)]
pub struct Killer(Arc<Mutex<Child>>);

impl Killer {
    pub fn kill(&self) {
        if let Ok(mut child) = self.0.lock() {
            let _ = child.kill();
        }
    }
}

/// `forge worker` status, as nested in [`Snapshot`]: `{"running": false}`
/// when no worker pid file exists.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Worker {
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub pid: i64,
    #[serde(default)]
    pub exe: String,
    #[serde(default)]
    pub stale_binary: bool,
}

/// The document `forge snapshot` prints.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Snapshot {
    #[serde(default)]
    pub tasks: Vec<TaskRow>,
    #[serde(default)]
    pub requests: Vec<RequestRow>,
    #[serde(default)]
    pub worker: Worker,
    #[serde(default)]
    pub events_offset: u64,
}

/// One row of `forge log --json`: a task as the queue lists it.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TaskRow {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub workflow: String,
    #[serde(default)]
    pub attempts: i64,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub created: String,
}

/// One row of `forge requests --json`: a blocked task and what it is
/// waiting on.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RequestRow {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tried: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub workflow: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub task: String,
}

/// One row of `forge decisions --json`: an operator's or the supervisor's
/// answer to a blocked task's question.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DecisionRow {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub task_id: i64,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub answer: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub answered_by: String,
    #[serde(default)]
    pub citations: String,
    #[serde(default)]
    pub retry_id: Option<i64>,
    #[serde(default)]
    pub outcome: Option<String>,
}

/// Token counts from an attempt's result frame, as `TraceDoc` nests them.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Tokens {
    #[serde(default)]
    pub input: Option<i64>,
    #[serde(default)]
    pub output: Option<i64>,
    #[serde(default)]
    pub cache_read: Option<i64>,
    #[serde(default)]
    pub cache_creation: Option<i64>,
}

/// Rate-limit usage samples from an attempt, as `TraceDoc` nests them.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RateLimits {
    #[serde(default)]
    pub five_hour: Option<f64>,
    #[serde(default)]
    pub seven_day: Option<f64>,
}

/// One entry of `TraceDoc.attempts`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Attempt {
    #[serde(default)]
    pub attempt_no: i64,
    #[serde(default)]
    pub step: String,
    #[serde(default)]
    pub step_seq: i64,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub started_at: i64,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub agent_exit: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
    #[serde(default)]
    pub num_turns: i64,
    #[serde(default)]
    pub tool_calls: i64,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub agent_ms: i64,
    #[serde(default)]
    pub commits: i64,
    #[serde(default)]
    pub files_changed: i64,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default)]
    pub start_sha: String,
    #[serde(default)]
    pub end_sha: String,
    #[serde(default)]
    pub log_path: String,
    #[serde(default)]
    pub tokens: Tokens,
    #[serde(default)]
    pub rate_limits: RateLimits,
    #[serde(default)]
    pub inputs: Value,
    #[serde(default)]
    pub outputs: Value,
    #[serde(default)]
    pub verdict: Value,
    #[serde(default)]
    pub envelope: Value,
}

/// One entry of `TraceDoc.ops`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Op {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub seq: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kernel: bool,
    #[serde(default)]
    pub started_at: i64,
    #[serde(default)]
    pub ms: i64,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub exit: Option<i32>,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub attempt_id: Option<i64>,
    #[serde(default)]
    pub output: String,
}

/// The document `forge trace ID --json` prints: everything about one task.
/// `task`, `resolved` and `diagnosis` stay raw JSON — `task` because
/// `docs/CLIENT.md` documents it field-by-field but it is large and mostly
/// store columns passed through unchanged, `resolved` because it is
/// `workflows::Resolved` (a kernel type this crate does not depend on), and
/// `diagnosis` because it is a short, simple `{what, action}` list a caller
/// can read straight off the `Value`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TraceDoc {
    #[serde(default)]
    pub task: Value,
    #[serde(default)]
    pub attempts: Vec<Attempt>,
    #[serde(default)]
    pub ops: Vec<Op>,
    #[serde(default)]
    pub resolved: Value,
    #[serde(default)]
    pub diagnosis: Value,
}

/// One workflow's declared metadata and measured outcomes, one entry of
/// `forge workflows --json`'s `"workflows"` array. Its shape is informal
/// (see `docs/CLIENT.md`: "no client reads it today"), so the nested
/// pieces stay raw JSON.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Workflow {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub steps: Value,
    #[serde(default)]
    pub resolved: Value,
    #[serde(default)]
    pub meta: Value,
    #[serde(default)]
    pub measured: Value,
}

/// One line of `forge events`/`forge events --follow`: the fields of one
/// `report::Event` variant, tagged by `type`, plus `text`, `ts` and `task`
/// on every variant. A `type` this crate doesn't recognize (an older
/// binary talking to a newer one, or vice versa) lands in `Other` instead
/// of failing to parse.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    TaskStarted {
        #[serde(default)]
        worktree: String,
        #[serde(default)]
        branch: String,
        #[serde(default)]
        base_branch: String,
        #[serde(default)]
        base_sha: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        max_turns: i64,
        #[serde(default)]
        max_attempts: i64,
        #[serde(default)]
        timeout_secs: i64,
        #[serde(default)]
        sandboxed: bool,
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    TaskQueued {
        #[serde(default)]
        workflow: String,
        #[serde(default)]
        retry_of: Option<i64>,
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    AttemptStarted {
        #[serde(default)]
        n: i64,
        #[serde(default)]
        of: i64,
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    ToolCall {
        #[serde(default)]
        name: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    AgentDone {
        #[serde(default)]
        exit: Option<i32>,
        #[serde(default)]
        turns: i64,
        #[serde(default)]
        tools: i64,
        #[serde(default)]
        ms: i64,
        #[serde(default)]
        cost_usd: Option<f64>,
        #[serde(default)]
        timed_out: bool,
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    GitCounted {
        #[serde(default)]
        commits: i64,
        #[serde(default)]
        files: i64,
        #[serde(default)]
        dirty: bool,
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    Check {
        #[serde(default)]
        level: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        ok: bool,
        #[serde(default)]
        ms: i64,
        #[serde(default)]
        tail: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    AttemptDone {
        #[serde(default)]
        state: String,
        #[serde(default)]
        reason: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    Pushed {
        #[serde(default)]
        remote: String,
        #[serde(default)]
        branch: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    PushFailed {
        #[serde(default)]
        error: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    PushSkipped {
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    TaskDone {
        #[serde(default)]
        state: String,
        #[serde(default)]
        attempts: i64,
        #[serde(default)]
        cost_usd: f64,
        #[serde(default)]
        reason: String,
        #[serde(default)]
        branch: String,
        #[serde(default)]
        pushed: bool,
        #[serde(default)]
        compare: Option<String>,
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    Note {
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    Op {
        #[serde(default)]
        name: String,
        #[serde(default)]
        kernel: bool,
        #[serde(default)]
        ok: bool,
        #[serde(default)]
        ms: i64,
        #[serde(default)]
        detail: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    #[serde(other)]
    Other,
}
