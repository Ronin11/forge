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
#[derive(Clone)]
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

    /// `forge plugin list --json`: every plugin found, where it came from,
    /// and whether it is enabled.
    pub fn plugin_list(&self) -> Result<Vec<PluginRow>> {
        let v = self.json(&["plugin", "list", "--json"])?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge plugin status --json`: every plugin's enabled flag and
    /// running state.
    pub fn plugin_status(&self) -> Result<Vec<PluginStatusRow>> {
        let v = self.json(&["plugin", "status", "--json"])?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge project list --json`: every project, alphabetically.
    pub fn project_list(&self) -> Result<Vec<ProjectRow>> {
        let v = self.json(&["project", "list", "--json"])?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge project show NAME --json`: one project.
    pub fn project_show(&self, name: &str) -> Result<ProjectRow> {
        let v = self.json(&["project", "show", name, "--json"])?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge project backlog NAME --json`: one project's backlog, oldest
    /// first.
    pub fn project_backlog(&self, name: &str) -> Result<Vec<BacklogRow>> {
        let v = self.json(&["project", "backlog", name, "--json"])?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge initiative list [<project>] --json`: every initiative, or
    /// only `project`'s.
    pub fn initiative_list(&self, project: Option<&str>) -> Result<Vec<InitiativeRow>> {
        let mut args = vec!["initiative", "list"];
        if let Some(p) = project {
            args.push(p);
        }
        args.push("--json");
        let v = self.json(&args)?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge initiative show ID --json`: one initiative.
    pub fn initiative_show(&self, id: i64) -> Result<InitiativeRow> {
        let id = id.to_string();
        let v = self.json(&["initiative", "show", &id, "--json"])?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge initiative report ID --json`: the generated report.
    pub fn initiative_report(&self, id: i64) -> Result<InitiativeDoc> {
        let id = id.to_string();
        let v = self.json(&["initiative", "report", &id, "--json"])?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge stats --json`: see [`StatsDoc`].
    pub fn stats(&self) -> Result<StatsDoc> {
        let v = self.json(&["stats", "--json"])?;
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

/// Runs a tool other than `forge` itself — resolved on `PATH`, exactly as
/// the shell would — and returns its stdout as text, or an error carrying
/// stderr on a non-zero exit: the same contract as [`Forge::run`], for the
/// rest of Forge's own toolchain (`forge-repomap`, today). A client spawns
/// these directly rather than through a `forge` verb because they aren't
/// part of the kernel's contract with a client; `docs/CLIENT.md` doesn't
/// describe them.
pub fn spawn(bin: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(bin)
        .args(args)
        .output()
        .with_context(|| format!("running {bin} {}", args.join(" ")))?;
    if !out.status.success() {
        anyhow::bail!(
            "{bin} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// [`spawn`], then parsed as one JSON value.
pub fn spawn_json(bin: &str, args: &[&str]) -> Result<Value> {
    let text = spawn(bin, args)?;
    serde_json::from_str(&text).with_context(|| format!("parsing {bin} {}", args.join(" ")))
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
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub initiative: Option<i64>,
}

/// One row of `forge requests --json`: a blocked task and what it is
/// waiting on.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RequestRow {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub kind: String,
    /// Who the question is addressed to; `None` means the operator.
    #[serde(default)]
    pub to: Option<String>,
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

/// One row of `forge plugin list --json`: a plugin as discovered.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PluginRow {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub dir: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub restart: String,
    #[serde(default)]
    pub enabled: bool,
}

/// One row of `forge plugin status --json`: whether a plugin is enabled
/// and, per the supervisor's last record, whether it is `running` (with
/// `pid`/`uptime_secs`), `restarting` (with `restart_count`), or `stopped`
/// (with `last_exit`).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PluginStatusRow {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub pid: Option<i64>,
    #[serde(default)]
    pub uptime_secs: Option<i64>,
    #[serde(default)]
    pub restart_count: Option<u32>,
    #[serde(default)]
    pub last_exit: Option<String>,
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
    /// Who the question was addressed to; `None` means the operator.
    #[serde(default)]
    pub answered_for: Option<String>,
}

/// One repository listed under a project, and the paths it owns there
/// (`None` scope means the whole repository), nested in [`ProjectRow`].
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProjectRepoRow {
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub scope: Option<String>,
}

/// One row of `forge project list --json` / `forge project show --json`:
/// a project, the repositories it works in, task counts by state, cost,
/// and its own defaults.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProjectRow {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub repos: Vec<ProjectRepoRow>,
    #[serde(default)]
    pub queued: i64,
    #[serde(default)]
    pub running: i64,
    #[serde(default)]
    pub succeeded: i64,
    #[serde(default)]
    pub failed: i64,
    #[serde(default)]
    pub unverified: i64,
    #[serde(default)]
    pub blocked: i64,
    #[serde(default)]
    pub withdrawn: i64,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub workflow: Option<String>,
    #[serde(default)]
    pub per_task_usd: Option<f64>,
    #[serde(default)]
    pub per_initiative_usd: Option<f64>,
    #[serde(default)]
    pub supervisor_model: Option<String>,
    #[serde(default)]
    pub supervisor_per_lineage: Option<i64>,
    #[serde(default)]
    pub protected: Vec<String>,
}

/// One row of `forge project backlog NAME --json`: a thing worth doing
/// that is not yet queued.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct BacklogRow {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub done_at: Option<i64>,
}

/// One row of `forge initiative list --json` / `forge initiative show
/// --json`: an initiative, its derived state, task counts by state, cost
/// and its own settings.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitiativeRow {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub held_rule: Option<String>,
    #[serde(default)]
    pub queued: i64,
    #[serde(default)]
    pub running: i64,
    #[serde(default)]
    pub succeeded: i64,
    #[serde(default)]
    pub failed: i64,
    #[serde(default)]
    pub unverified: i64,
    #[serde(default)]
    pub blocked: i64,
    #[serde(default)]
    pub withdrawn: i64,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub budget_usd: Option<f64>,
    #[serde(default)]
    pub stop_after_same_rule: i64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub settled_at: Option<i64>,
}

/// One lineage in [`InitiativeDoc::tasks`]: its latest task's id, state
/// and reason, plus how many retries the lineage took to reach it.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitiativeTaskRow {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub retries: i64,
}

/// One row of `InitiativeDoc.refused`: a verification rule name and how
/// many attempts of the initiative's tasks it refused.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RefusedRow {
    #[serde(default)]
    pub rule: String,
    #[serde(default)]
    pub count: i64,
}

/// One row of `InitiativeDoc.rulings`: a decision the supervisor made on
/// one of the initiative's tasks.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitiativeRulingRow {
    #[serde(default)]
    pub task_id: i64,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub answer: String,
    #[serde(default)]
    pub citations: String,
}

/// One row of `InitiativeDoc.questions`: a question that reached the
/// operator, answered or (while the task is still blocked) not yet.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitiativeQuestionRow {
    #[serde(default)]
    pub task_id: i64,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub answer: Option<String>,
}

/// One row of `InitiativeDoc.deployed`: a deploy one of the initiative's
/// tasks triggered on landing.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitiativeDeployRow {
    #[serde(default)]
    pub task_id: i64,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub check_ok: Option<bool>,
    #[serde(default)]
    pub rolled_back_to: Option<String>,
}

/// The document `forge initiative report ID --json` prints: the outcome,
/// each task and how it ended, what verification refused, what the
/// supervisor ruled, what reached the operator, cost and elapsed time.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitiativeDoc {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub held_rule: Option<String>,
    #[serde(default)]
    pub budget_usd: Option<f64>,
    #[serde(default)]
    pub stop_after_same_rule: i64,
    #[serde(default)]
    pub tasks: Vec<InitiativeTaskRow>,
    #[serde(default)]
    pub refused: Vec<RefusedRow>,
    #[serde(default)]
    pub rulings: Vec<InitiativeRulingRow>,
    #[serde(default)]
    pub questions: Vec<InitiativeQuestionRow>,
    #[serde(default)]
    pub deployed: Vec<InitiativeDeployRow>,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub elapsed_secs: Option<i64>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub settled_at: Option<i64>,
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

/// One entry of `TraceDoc.deploys`: one deploy the task triggered on
/// landing.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Deploy {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub started_at: i64,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub check_ok: Option<bool>,
    #[serde(default)]
    pub check_output: String,
    #[serde(default)]
    pub rolled_back_to: Option<String>,
    #[serde(default)]
    pub reason: String,
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
    #[serde(default)]
    pub deploys: Vec<Deploy>,
}

/// One row of `StatsDoc.by_role`: attempts, outcomes, cost and wall time
/// for one (role, provider, model) combination, role being the attempt's
/// step (`code`, `review`, and so on). `landed`, `broke_base` and
/// `broke_base_share` are only ever present for the `code` role.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct StatsRoleRow {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub attempts: i64,
    #[serde(default)]
    pub succeeded: i64,
    #[serde(default)]
    pub succeeded_share: Option<f64>,
    #[serde(default)]
    pub mean_turns: f64,
    #[serde(default)]
    pub mean_cost_usd: f64,
    #[serde(default)]
    pub mean_secs: f64,
    #[serde(default)]
    pub landed: Option<i64>,
    #[serde(default)]
    pub broke_base: Option<i64>,
    #[serde(default)]
    pub broke_base_share: Option<f64>,
}

/// The document `forge stats --json` prints. `docs/CLIENT.md` documents
/// the full shape (`workflows`, `steps`, `journal`, `no_journal`,
/// `projects`, `by_role`, `tools`); only `by_role` is modeled here today,
/// the rest added as a client needs them.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct StatsDoc {
    #[serde(default)]
    pub by_role: Vec<StatsRoleRow>,
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
