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
use std::io::{BufRead, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

/// Retries a spawn a few times on `ETXTBSY`: the kernel can transiently
/// report a just-written, just-chmod'd binary as busy under heavy
/// concurrent process load, before any process actually holds it open.
/// Nothing has run yet when this fires, so retrying is safe.
fn retry_on_etxtbsy<T>(mut spawn: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    for attempt in 0..5 {
        match spawn() {
            Err(e) if e.raw_os_error() == Some(26) && attempt < 4 => {
                std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
            }
            result => return result,
        }
    }
    unreachable!()
}

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
        let out = retry_on_etxtbsy(|| Command::new(&self.bin).args(args).output())
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

    /// `forge project view NAME --json`: everything the customer portal's
    /// page needs for one project.
    pub fn project_view(&self, name: &str) -> Result<PortalDoc> {
        let v = self.json(&["project", "view", name, "--json"])?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge project resolve-token TOKEN --json`: the project a customer
    /// portal token opens. An error means the token is unknown or
    /// revoked (see docs/PORTAL.md); the portal server turns that into a
    /// plain 404 page.
    pub fn resolve_portal_token(&self, token: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct Resolved {
            project: String,
        }
        let v = self.json(&["project", "resolve-token", token, "--json"])?;
        let r: Resolved = serde_json::from_value(v)?;
        Ok(r.project)
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

    /// `forge job list [<project>] --json`: jobs, newest first, or only
    /// `project`'s.
    pub fn job_list(&self, project: Option<&str>) -> Result<Vec<JobRow>> {
        let mut args = vec!["job", "list"];
        if let Some(p) = project {
            args.push(p);
        }
        args.push("--json");
        let v = self.json(&args)?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge job show ID --json`: one job, with every step and effect it
    /// recorded.
    pub fn job_show(&self, id: i64) -> Result<JobDoc> {
        let id = id.to_string();
        let v = self.json(&["job", "show", &id, "--json"])?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge job log <project> --json`: a project's job effects across
    /// every one of its jobs, newest first.
    pub fn job_log(&self, project: &str) -> Result<Vec<JobEffectRow>> {
        let v = self.json(&["job", "log", project, "--json"])?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge deploy log <project> [<target>] --json`: a project's deploys,
    /// newest first, each a [`Deploy`]: the rows `TraceDoc.deploys` carries,
    /// with the smoke and look verdicts.
    pub fn deploy_log(&self, project: &str, target: Option<&str>) -> Result<Vec<Deploy>> {
        let mut args = vec!["deploy", "log", project];
        if let Some(t) = target {
            args.push(t);
        }
        args.push("--json");
        let v = self.json(&args)?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge graph REPO --json`: the module graph for a repository at
    /// its working tree, with the record's overlay on each file node.
    pub fn graph(&self, repo: &str) -> Result<GraphDoc> {
        let v = self.json(&["graph", repo, "--json"])?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge workflows --json [--project NAME]`: the operator catalog, or,
    /// with `project`, the catalog plus that project's own repository
    /// workflows (`source: "repo"` entries), for the `/workflows` list page.
    pub fn workflow_list(&self, project: Option<&str>) -> Result<Vec<Workflow>> {
        let mut args = vec!["workflows"];
        if let Some(p) = project {
            args.push("--project");
            args.push(p);
        }
        args.push("--json");
        let v = self.json(&args)?;
        #[derive(Deserialize, Default)]
        struct Doc {
            #[serde(default)]
            workflows: Vec<Workflow>,
        }
        let doc: Doc = serde_json::from_value(v)?;
        Ok(doc.workflows)
    }

    /// `forge workflows show NAME [--project NAME] --json`: one workflow in
    /// full — its file text, where it came from, kind, every resolved
    /// step, and its measured profile. `project` finds a repository
    /// workflow (a project's own `.forge/workflows/`) when the operator
    /// catalog has none of this name.
    pub fn workflow_show(&self, name: &str, project: Option<&str>) -> Result<WorkflowShowDoc> {
        let mut args = vec!["workflows", "show", name];
        if let Some(p) = project {
            args.push("--project");
            args.push(p);
        }
        args.push("--json");
        let v = self.json(&args)?;
        Ok(serde_json::from_value(v)?)
    }

    /// `forge workflows lint --stdin [--name NAME]`: validate a candidate
    /// workflow file's `text` against the catalog without writing it
    /// anywhere, so an editor can check as the operator types. Unlike
    /// [`Forge::run`]/[`Forge::json`], a non-zero exit here (1, on any lint
    /// problem) is the answer, not a failure, so this bypasses both and
    /// spawns directly to pipe `text` in on stdin.
    pub fn workflow_lint(
        &self,
        name: Option<&str>,
        text: &str,
    ) -> Result<Vec<WorkflowLintProblem>> {
        let mut args = vec!["workflows", "lint", "--stdin"];
        if let Some(n) = name {
            args.push("--name");
            args.push(n);
        }
        let mut child = retry_on_etxtbsy(|| {
            Command::new(&self.bin)
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        })
        .with_context(|| format!("running {} {}", self.bin, args.join(" ")))?;
        child
            .stdin
            .take()
            .context("lint stdin")?
            .write_all(text.as_bytes())
            .context("writing the candidate text to forge workflows lint")?;
        let out = child.wait_with_output().context("forge workflows lint")?;
        if !out.status.success() && out.status.code() != Some(1) {
            anyhow::bail!(
                "forge workflows lint: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        #[derive(Deserialize)]
        struct Doc {
            #[serde(default)]
            problems: Vec<WorkflowLintProblem>,
        }
        let doc: Doc = serde_json::from_slice(&out.stdout)
            .with_context(|| format!("parsing forge {}", args.join(" ")))?;
        Ok(doc.problems)
    }

    /// `forge workflows put NAME --stdin --message MESSAGE [--repo PATH]`:
    /// write verb. Writes `text` into the operator's catalog once it
    /// lints clean and commits it there, returning the new commit hash;
    /// or, with `repo`, files a direct task on that repository's project
    /// that lands the same content through review instead of a direct
    /// write, returning the new task's id. Unlike
    /// [`Forge::workflow_lint`], a non-zero exit here — a lint failure, a
    /// `NAME` that doesn't match the candidate's own declared `name`, or
    /// an empty `message` — is the error, so this goes through the same
    /// contract as [`Forge::run`] except for piping `text` in on stdin.
    pub fn workflow_put(
        &self,
        name: &str,
        text: &str,
        message: &str,
        repo: Option<&str>,
    ) -> Result<WorkflowPutResult> {
        let mut args = vec!["workflows", "put", name, "--stdin", "--message", message];
        if let Some(r) = repo {
            args.push("--repo");
            args.push(r);
        }
        let mut child = retry_on_etxtbsy(|| {
            Command::new(&self.bin)
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        })
        .with_context(|| format!("running {} {}", self.bin, args.join(" ")))?;
        child
            .stdin
            .take()
            .context("put stdin")?
            .write_all(text.as_bytes())
            .context("writing the candidate text to forge workflows put")?;
        let out = child.wait_with_output().context("forge workflows put")?;
        if !out.status.success() {
            anyhow::bail!(
                "forge workflows put: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(match repo {
            Some(_) => WorkflowPutResult::Filed {
                task_id: stdout.parse().with_context(|| {
                    format!("forge workflows put printed a non-numeric task id: {stdout:?}")
                })?,
            },
            None => WorkflowPutResult::Committed { hash: stdout },
        })
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
        let mut child = retry_on_etxtbsy(|| {
            Command::new(&self.bin)
                .args(["events", "--since", &offset.to_string(), "--follow"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
        })
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
    /// Jobs started in the last rolling 24h, counted separately from the
    /// task counts above (docs/JOBS.md step 1d).
    #[serde(default)]
    pub jobs_today: i64,
    #[serde(default)]
    pub jobs_ok: i64,
    #[serde(default)]
    pub jobs_failed: i64,
    #[serde(default)]
    pub jobs_needs_human: i64,
    #[serde(default)]
    pub jobs_skipped: i64,
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

/// One deploy target on [`PortalDoc`]: the customer's "Running for you"
/// list (see docs/PORTAL.md, "What they see").
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalDeployTarget {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub where_it_runs: String,
    #[serde(default)]
    pub last_deployed_at: Option<i64>,
    #[serde(default)]
    pub check_ok: Option<bool>,
    #[serde(default)]
    pub look_ok: Option<bool>,
    #[serde(default)]
    pub screenshot: Option<String>,
}

/// One of a run workflow's last three jobs on [`PortalDoc`], "Running
/// for you" continued: when it ran, whether it went `"ok"`, `"failed"`
/// or `"needs_human"`, and, on failure, a one-line reason.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalJobRun {
    #[serde(default)]
    pub started_at: i64,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// One run workflow on [`PortalDoc`]: an automation this project's jobs
/// run through, and its last three jobs, newest first.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalWorkflow {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub jobs: Vec<PortalJobRun>,
}

/// One open initiative on [`PortalDoc`]: the customer's "Being built"
/// list, newest first, capped at ten (`PortalDoc.initiatives_more` the
/// rest). `state` is always "in progress" or "waiting on you"; `pieces`
/// how many tasks make up the initiative so far.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalInitiative {
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub pieces: i64,
}

/// One open question on [`PortalDoc`], addressed to the customer: the
/// "Needs you" list; `asked_at` is Unix seconds, when the task blocked on
/// it (`None` from a `forge` that predates the field).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalQuestion {
    #[serde(default)]
    pub task_id: i64,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub asked_at: Option<i64>,
}

/// One line on [`PortalDoc`]'s "Done" list, newest first, capped at ten
/// (`PortalDoc.landed_more` the rest): a landed initiative's outcome
/// (`pieces` how many tasks it took), or a landed task belonging to no
/// initiative (`pieces` `None`), `text` its title or a line derived from
/// its request.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalLanded {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub pieces: Option<i64>,
    #[serde(default)]
    pub landed_at: i64,
}

/// The confirmed intake brief on [`PortalDoc`] (see docs/INTAKE.md):
/// "Your plan".
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalBrief {
    #[serde(default)]
    pub where_it_runs: String,
    #[serde(default)]
    pub workflows: Vec<String>,
}

/// One open backlog item on [`PortalDoc`].
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalBacklogItem {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub created_at: i64,
}

/// The document `forge project view NAME --json` prints: everything the
/// customer portal's page needs for one project, in their own words (see
/// docs/PORTAL.md, "What they see"). No branches, costs, attempt data or
/// verdict rows — those belong to the operator's page, not this one.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalDoc {
    #[serde(default)]
    pub project: String,
    /// The project's purpose; never rendered on the customer's page.
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub deploy_targets: Vec<PortalDeployTarget>,
    #[serde(default)]
    pub run_workflows: Vec<PortalWorkflow>,
    #[serde(default)]
    pub initiatives: Vec<PortalInitiative>,
    /// How many open initiatives past the ten in `initiatives`.
    #[serde(default)]
    pub initiatives_more: i64,
    #[serde(default)]
    pub questions: Vec<PortalQuestion>,
    #[serde(default)]
    pub landed: Vec<PortalLanded>,
    /// How many landed lines past the ten in `landed`.
    #[serde(default)]
    pub landed_more: i64,
    #[serde(default)]
    pub brief: Option<PortalBrief>,
    #[serde(default)]
    pub backlog: Vec<PortalBacklogItem>,
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
/// and reason, plus how many retries the lineage took to reach it, and
/// the assess directive's score for its own landing, if any ran (see
/// docs/ACTIONS.md, "Assessment").
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
    #[serde(default)]
    pub score: Option<i64>,
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

/// One row of `forge job list --json`: a job, one run of a `kind = "run"`
/// workflow (see docs/JOBS.md).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct JobRow {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub workflow: String,
    #[serde(default)]
    pub workflow_hash: String,
    #[serde(default)]
    pub landed_sha: String,
    #[serde(default)]
    pub trigger_kind: String,
    #[serde(default)]
    pub trigger_ref: String,
    #[serde(default)]
    pub state: String,
    /// `"repo"` or `"catalog"`: where the workflow was resolved from (see
    /// docs/JOBS.md, "Where an automation lives").
    #[serde(default)]
    pub workflow_source: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub started_at: i64,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub verdict_json: String,
    /// When this job becomes claimable, a unix second; `None` for a job
    /// that was never delayed (see docs/JOBS.md, "Delayed jobs").
    #[serde(default)]
    pub due_at: Option<i64>,
}

/// One row of `JobDoc.steps`: one step of a job's run.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct JobStepRow {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub job_id: i64,
    #[serde(default)]
    pub seq: i64,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub started_at: i64,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub output_ref: String,
    #[serde(default)]
    pub tail: String,
}

/// One row of `JobDoc.effects` and of `forge job log --json`: one effect a
/// job's step performed on the world.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct JobEffectRow {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub job_id: i64,
    #[serde(default)]
    pub seq: i64,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub dry_run: bool,
}

/// The document `forge job show ID --json` prints: one job with every step
/// and effect it recorded.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct JobDoc {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub workflow: String,
    #[serde(default)]
    pub workflow_hash: String,
    #[serde(default)]
    pub landed_sha: String,
    #[serde(default)]
    pub trigger_kind: String,
    #[serde(default)]
    pub trigger_ref: String,
    #[serde(default)]
    pub state: String,
    /// `"repo"` or `"catalog"`: where the workflow was resolved from (see
    /// docs/JOBS.md, "Where an automation lives").
    #[serde(default)]
    pub workflow_source: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub started_at: i64,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub verdict_json: String,
    /// When this job becomes claimable, a unix second; `None` for a job
    /// that was never delayed (see docs/JOBS.md, "Delayed jobs").
    #[serde(default)]
    pub due_at: Option<i64>,
    #[serde(default)]
    pub steps: Vec<JobStepRow>,
    #[serde(default)]
    pub effects: Vec<JobEffectRow>,
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
    #[serde(default)]
    pub smoke_ok: Option<bool>,
    #[serde(default)]
    pub smoke_json: Option<String>,
    #[serde(default)]
    pub look_ok: Option<bool>,
    #[serde(default)]
    pub look_json: Option<String>,
}

/// One finding in `Assessment.findings`, as the assess directive
/// returned it.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Finding {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub finding: String,
    #[serde(default)]
    pub severity: String,
}

/// `TraceDoc.assessment`: the assess directive's most recent run against
/// this task's own landing (see docs/ACTIONS.md, "Assessment"). Absent
/// when it never ran.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Assessment {
    #[serde(default)]
    pub score: i64,
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub created_at: i64,
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
    #[serde(default)]
    pub assessment: Option<Assessment>,
}

/// One row of `StatsDoc.by_role`: attempts, outcomes, cost and wall time
/// for one (role, provider, model, kind) combination, role being the
/// attempt's step (`code`, `review`, and so on) or a job's directive
/// step's action, and kind (`"attempt"` or `"job_step"`) distinguishing
/// the two. `landed`, `broke_base`, `broke_base_share`, `repair_cost_usd`,
/// `true_cost_per_landed_usd` and `churn_share` are only ever present for
/// the `code` role's `"attempt"` rows.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct StatsRoleRow {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub kind: String,
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
    #[serde(default)]
    pub repair_cost_usd: Option<f64>,
    #[serde(default)]
    pub true_cost_per_landed_usd: Option<f64>,
    #[serde(default)]
    pub churn_share: Option<f64>,
}

/// One row of `StatsDoc.factors`: one level of one factor in `forge
/// stats --factors` (docs/ECONOMIST.md, piece 3) — `factor` is
/// `"provider:<role>"`, `"workflow"`, or `"size"`; `level` is that
/// factor's value. `rate`/`rate_lo`/`rate_hi` is the landing rate with
/// its Wilson 95% interval; `mean_true_cost_usd` is null when nothing
/// in this level landed. `effect`/`effect_se` come from one joint
/// least-squares fit of `ln(true cost)` across every factor and level
/// in scope; both are null for the reference level (`is_reference`) and
/// for any level the fit could not identify. See `docs/CLIENT.md`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct StatsFactorRow {
    #[serde(default)]
    pub factor: String,
    #[serde(default)]
    pub level: String,
    #[serde(default)]
    pub tasks: i64,
    #[serde(default)]
    pub landed: i64,
    #[serde(default)]
    pub rate: f64,
    #[serde(default)]
    pub rate_lo: f64,
    #[serde(default)]
    pub rate_hi: f64,
    #[serde(default)]
    pub mean_true_cost_usd: Option<f64>,
    #[serde(default)]
    pub is_reference: bool,
    #[serde(default)]
    pub effect: Option<f64>,
    #[serde(default)]
    pub effect_se: Option<f64>,
}

/// The document `forge stats --json` prints. `docs/CLIENT.md` documents
/// the full shape (`workflows`, `steps`, `journal`, `no_journal`,
/// `projects`, `by_role`, `factors`, `tools`); only `by_role` and
/// `factors` are modeled here today, the rest added as a client needs
/// them.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct StatsDoc {
    #[serde(default)]
    pub by_role: Vec<StatsRoleRow>,
    #[serde(default)]
    pub factors: Vec<StatsFactorRow>,
}

/// The document `forge graph REPO --json` prints: the module graph
/// (docs/LATER.md, "The code visualiser") for a repository at its
/// working tree, with each file node's overlay from the record laid on
/// top (docs/LATER.md, "The overlay, from the record").
#[derive(Debug, Clone, Deserialize, Default)]
pub struct GraphDoc {
    #[serde(default)]
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub edges: Vec<GraphEdge>,
}

/// One node of `GraphDoc`: a source file `forge-repomap` extracts, or a
/// directory that groups files directly under it (`kind` is `"file"` or
/// `"module"`). `overlay` is only ever populated for a `"file"` node.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct GraphNode {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub symbols: i64,
    #[serde(default)]
    pub lines: i64,
    #[serde(default)]
    pub overlay: Overlay,
}

/// One edge of `GraphDoc`: an import that resolves to another file in
/// the tree.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct GraphEdge {
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
}

/// What the record knows about one file node (`GraphNode::overlay`):
/// empty for a file no task ever touched.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Overlay {
    #[serde(default)]
    pub tasks: Vec<TaskTouch>,
    #[serde(default)]
    pub demotions: Vec<Demotion>,
    #[serde(default)]
    pub repair_cost_usd: f64,
}

/// One task that touched a file node, from its attempts' recorded
/// `changes`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TaskTouch {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub at: i64,
    #[serde(default)]
    pub cost_usd: f64,
}

/// One review demotion whose evidence names a file node.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Demotion {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub at: i64,
    #[serde(default)]
    pub reason: String,
}

/// One workflow's declared metadata and measured outcomes, one entry of
/// `forge workflows --json`'s `"workflows"` array. Its shape is informal
/// (see `docs/CLIENT.md`: "no client reads it today"), so the nested
/// pieces stay raw JSON.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Workflow {
    #[serde(default)]
    pub name: String,
    /// `"build"` or `"run"`.
    #[serde(default)]
    pub kind: String,
    /// `"catalog"` (the operator's own) or `"repo"` (a project's own
    /// `.forge/workflows/`).
    #[serde(default)]
    pub source: String,
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

/// One resolved step of `forge workflows show --json`'s `"steps"` array:
/// the action's name, kind, contract, model, turns, timeout and
/// description, in the order the workflow runs them.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkflowStepDoc {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub contract: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub timeout_secs: Option<u32>,
    #[serde(default)]
    pub description: String,
}

/// The document `forge workflows show NAME --json` prints: one workflow in
/// full. `measured` stays raw JSON, the same informal shape as
/// [`Workflow::measured`].
#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkflowShowDoc {
    #[serde(default)]
    pub name: String,
    /// `"catalog"` (the operator's own) or `"repo"` (a project's own
    /// `.forge/workflows/`).
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub path: String,
    /// `"build"` or `"run"`.
    #[serde(default)]
    pub kind: String,
    /// The workflow file's exact text.
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub steps: Vec<WorkflowStepDoc>,
    #[serde(default)]
    pub measured: Value,
}

/// What `forge workflows put` did: a commit landed straight in the
/// operator's catalog, or, with `--repo`, a direct task was filed on a
/// repository's project instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowPutResult {
    Committed { hash: String },
    Filed { task_id: i64 },
}

/// One problem `forge workflows lint --stdin` found in a candidate
/// workflow file's text: the line the parser could place it at (`None` for
/// a semantic error found only after a clean parse), and the message.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkflowLintProblem {
    #[serde(default)]
    pub line: Option<usize>,
    #[serde(default)]
    pub message: String,
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
    JobStarted {
        #[serde(default)]
        project: String,
        #[serde(default)]
        workflow: String,
        #[serde(default)]
        job_id: i64,
        #[serde(default)]
        dry_run: bool,
        #[serde(default)]
        text: String,
        #[serde(default)]
        ts: i64,
        #[serde(default)]
        task: i64,
    },
    JobFinished {
        #[serde(default)]
        project: String,
        #[serde(default)]
        workflow: String,
        #[serde(default)]
        job_id: i64,
        #[serde(default)]
        state: String,
        #[serde(default)]
        cost_usd: f64,
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
