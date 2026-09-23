//! `forge-client`: the one Rust client of the `forge` CLI. It runs a
//! `forge` verb, parses its `--json` output, and hands back typed rows —
//! never the database, never the kernel. `docs/CLIENT.md` is the contract
//! this crate implements; every shape below is documented there.
//!
//! Two rules keep a broken contract loud instead of quiet:
//!
//! - Every run of the `forge` binary is held to [`Forge::timeout`]
//!   (default 60s). Past it the child is killed and the error names the
//!   verb, so a stuck subprocess can never strand a caller forever.
//! - A field is `#[serde(default)]` only when `docs/CLIENT.md` marks it
//!   optional (nullable, or documented as sometimes absent). Every other
//!   field is required: a `forge` binary that omits one fails the parse
//!   with an error naming the field and the verb, rather than silently
//!   handing back a zero, an empty string, or `false`.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often [`Forge::wait_with_deadline`] polls a child for exit, while
/// its stdout/stderr drain on background threads so a full pipe buffer
/// can never be why it looks stuck.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

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
    /// How long a run of [`Forge::bin`] is allowed before it is killed.
    /// Default 60s (see [`Forge::new`]); every verb below is held to it,
    /// since none of them are meant to run long — `forge events --follow`
    /// ([`Forge::subscribe`]) is the one deliberately unbounded process
    /// this crate starts, and it is not run through this deadline.
    pub timeout: Duration,
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
            timeout: Duration::from_secs(60),
        }
    }

    /// Spawns `forge <args>` with piped stdout/stderr and waits for it,
    /// draining both pipes on background threads so neither can fill up
    /// and stall the child while it waits. Past `self.timeout`, the child
    /// is killed and the error names the verb.
    fn wait_with_deadline(&self, mut child: Child, args: &[&str]) -> Result<std::process::Output> {
        fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_end(&mut buf);
                }
                buf
            })
        }
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .with_context(|| format!("waiting for forge {}", args.join(" ")))?
            {
                break status;
            }
            if start.elapsed() >= self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!(
                    "forge {}: timed out after {:?}",
                    args.join(" "),
                    self.timeout
                );
            }
            std::thread::sleep(POLL_INTERVAL);
        };
        Ok(std::process::Output {
            status,
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        })
    }

    /// Spawns `forge <args>` with piped stdin/stdout/stderr, for a verb
    /// that feeds a candidate on stdin ([`Forge::workflow_lint`],
    /// [`Forge::workflow_put`]).
    fn spawn_piped(&self, args: &[&str]) -> Result<Child> {
        retry_on_etxtbsy(|| {
            Command::new(&self.bin)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        })
        .with_context(|| format!("running {} {}", self.bin, args.join(" ")))
    }

    /// Runs `forge <args>` to completion under [`Forge::timeout`],
    /// returning its output regardless of exit code.
    fn output(&self, args: &[&str]) -> Result<std::process::Output> {
        let child = retry_on_etxtbsy(|| {
            Command::new(&self.bin)
                .args(args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        })
        .with_context(|| format!("running {} {}", self.bin, args.join(" ")))?;
        self.wait_with_deadline(child, args)
    }

    /// Runs `forge <args>`, returning stdout as text. A non-zero exit is
    /// an error carrying stderr, per `docs/CLIENT.md`: a client shows the
    /// error and never parses stdout in that case.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        let out = self.output(args)?;
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

    /// Parses `v` — already read from `forge <args>`'s stdout — as `T`,
    /// naming both the verb and the missing/mistyped field on failure,
    /// per the optional-field rule at the top of this file.
    fn parse<T: DeserializeOwned>(&self, args: &[&str], v: Value) -> Result<T> {
        serde_json::from_value(v).with_context(|| format!("parsing forge {}", args.join(" ")))
    }

    /// `forge snapshot`: the whole state at one instant, plus the point in
    /// the event log to subscribe from.
    pub fn snapshot(&self) -> Result<Snapshot> {
        let v = self.json(&["snapshot"])?;
        self.parse(&["snapshot"], v)
    }

    /// `forge doctor --json`: every check the CLI's own doctor runs. Not
    /// part of the stable contract (docs/CLIENT.md), but the web header
    /// strip reads it for the worker's rate windows and today's spend. An
    /// exit code of 1 here means a check is FAIL, not that the run
    /// failed — that is data, not an error — so this reads stdout
    /// regardless of the exit code and only errors if it isn't JSON.
    pub fn doctor(&self) -> Result<Vec<DoctorCheck>> {
        let args = ["doctor", "--json"];
        let out = self.output(&args)?;
        let v: Value = serde_json::from_slice(&out.stdout)
            .with_context(|| format!("parsing {} doctor --json", self.bin))?;
        self.parse(&args, v)
    }

    /// `forge plugin list --json`: every plugin found, where it came from,
    /// and whether it is enabled.
    pub fn plugin_list(&self) -> Result<Vec<PluginRow>> {
        let args = ["plugin", "list", "--json"];
        let v = self.json(&args)?;
        self.parse(&args, v)
    }

    /// `forge plugin status --json`: every plugin's enabled flag and
    /// running state.
    pub fn plugin_status(&self) -> Result<Vec<PluginStatusRow>> {
        let args = ["plugin", "status", "--json"];
        let v = self.json(&args)?;
        self.parse(&args, v)
    }

    /// `forge project list --json`: every project, alphabetically.
    pub fn project_list(&self) -> Result<Vec<ProjectRow>> {
        let args = ["project", "list", "--json"];
        let v = self.json(&args)?;
        self.parse(&args, v)
    }

    /// `forge project show NAME --json`: one project.
    pub fn project_show(&self, name: &str) -> Result<ProjectRow> {
        let args = ["project", "show", name, "--json"];
        let v = self.json(&args)?;
        self.parse(&args, v)
    }

    /// `forge project backlog NAME --json`: one project's backlog, oldest
    /// first.
    pub fn project_backlog(&self, name: &str) -> Result<Vec<BacklogRow>> {
        let args = ["project", "backlog", name, "--json"];
        let v = self.json(&args)?;
        self.parse(&args, v)
    }

    /// `forge project view NAME --json`: everything the customer portal's
    /// page needs for one project.
    pub fn project_view(&self, name: &str) -> Result<PortalDoc> {
        let args = ["project", "view", name, "--json"];
        let v = self.json(&args)?;
        self.parse(&args, v)
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
        let args = ["project", "resolve-token", token, "--json"];
        let v = self.json(&args)?;
        let r: Resolved = self.parse(&args, v)?;
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
        self.parse(&args, v)
    }

    /// `forge initiative show ID --json`: one initiative.
    pub fn initiative_show(&self, id: i64) -> Result<InitiativeRow> {
        let id = id.to_string();
        let args = ["initiative", "show", &id, "--json"];
        let v = self.json(&args)?;
        self.parse(&args, v)
    }

    /// `forge initiative report ID --json`: the generated report.
    pub fn initiative_report(&self, id: i64) -> Result<InitiativeDoc> {
        let id = id.to_string();
        let args = ["initiative", "report", &id, "--json"];
        let v = self.json(&args)?;
        self.parse(&args, v)
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
        self.parse(&args, v)
    }

    /// `forge job show ID --json`: one job, with every step and effect it
    /// recorded.
    pub fn job_show(&self, id: i64) -> Result<JobDoc> {
        let id = id.to_string();
        let args = ["job", "show", &id, "--json"];
        let v = self.json(&args)?;
        self.parse(&args, v)
    }

    /// `forge job log <project> --json`: a project's job effects across
    /// every one of its jobs, newest first.
    pub fn job_log(&self, project: &str) -> Result<Vec<JobEffectRow>> {
        let args = ["job", "log", project, "--json"];
        let v = self.json(&args)?;
        self.parse(&args, v)
    }

    /// `forge project deploy list <project> --json`: a project's deploy
    /// targets, alphabetically, each a [`DeployTargetRow`] (see
    /// docs/DEPLOY.md, "A target").
    pub fn deploy_targets(&self, project: &str) -> Result<Vec<DeployTargetRow>> {
        let args = ["project", "deploy", "list", project, "--json"];
        let v = self.json(&args)?;
        self.parse(&args, v)
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
        self.parse(&args, v)
    }

    /// `forge graph REPO --json`: the module graph for a repository at
    /// its working tree, with the record's overlay on each file node.
    pub fn graph(&self, repo: &str) -> Result<GraphDoc> {
        let args = ["graph", repo, "--json"];
        let v = self.json(&args)?;
        self.parse(&args, v)
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
        #[derive(Deserialize)]
        struct Doc {
            workflows: Vec<Workflow>,
        }
        let doc: Doc = self.parse(&args, v)?;
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
        self.parse(&args, v)
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
        let mut child = self.spawn_piped(&args)?;
        child
            .stdin
            .take()
            .context("lint stdin")?
            .write_all(text.as_bytes())
            .context("writing the candidate text to forge workflows lint")?;
        let out = self.wait_with_deadline(child, &args)?;
        if !out.status.success() && out.status.code() != Some(1) {
            anyhow::bail!(
                "forge workflows lint: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        #[derive(Deserialize)]
        struct Doc {
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
        let mut child = self.spawn_piped(&args)?;
        child
            .stdin
            .take()
            .context("put stdin")?
            .write_all(text.as_bytes())
            .context("writing the candidate text to forge workflows put")?;
        let out = self.wait_with_deadline(child, &args)?;
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
        let args = ["stats", "--json"];
        let v = self.json(&args)?;
        self.parse(&args, v)
    }

    /// `forge events --since <offset> --follow`, as an iterator of typed
    /// events. The subordinate process is killed when the iterator is
    /// dropped.
    pub fn subscribe(&self, offset: u64) -> Result<Subscription> {
        let mut command = Command::new(&self.bin);
        command
            .args(["events", "--since", &offset.to_string(), "--follow"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Drop cannot run when the owning process is killed. Linux also
        // ties the follower to its spawning thread, covering abrupt exits.
        #[cfg(target_os = "linux")]
        unsafe {
            use std::os::unix::process::CommandExt;
            let parent = libc::getpid();
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
        let mut child = retry_on_etxtbsy(|| command.spawn())
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
    /// Read an unmodified event line, preserving unknown event types for proxies.
    pub fn next_line(&mut self) -> Option<std::io::Result<String>> {
        self.lines.next()
    }

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
        self.killer().kill();
    }
}

/// See [`Subscription::killer`].
#[derive(Clone)]
pub struct Killer(Arc<Mutex<Child>>);

impl Killer {
    pub fn kill(&self) {
        if let Ok(mut child) = self.0.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// `forge worker` status, as nested in [`Snapshot`]: `{"running": false}`
/// when no worker pid file exists.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Worker {
    pub running: bool,
    #[serde(default)]
    pub pid: i64,
    #[serde(default)]
    pub exe: String,
    #[serde(default)]
    pub stale_binary: bool,
}

/// The document `forge snapshot` prints. Every field here is always
/// sent; none are optional.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Snapshot {
    pub tasks: Vec<TaskRow>,
    pub requests: Vec<RequestRow>,
    pub worker: Worker,
    pub events_offset: u64,
}

/// One row of `forge doctor --json`: `{name, status, detail, hint}`, plus
/// optional structured numbers a few checks carry alongside their prose
/// (`src/doctor.rs`'s `Check`) — `rate_limit` sets `provider` and the
/// window fields, `spend` sets `spend_usd`/`spend_cap_usd`, `queue` sets
/// `queued`/`running`, `worktrees` sets `worktree_ids`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DoctorCheck {
    pub name: String,
    pub status: String,
    pub detail: String,
    pub hint: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub five_hour_pct: Option<f64>,
    #[serde(default)]
    pub five_hour_resets_at: Option<i64>,
    #[serde(default)]
    pub seven_day_pct: Option<f64>,
    #[serde(default)]
    pub seven_day_resets_at: Option<i64>,
    #[serde(default)]
    pub spend_usd: Option<f64>,
    #[serde(default)]
    pub spend_cap_usd: Option<f64>,
    #[serde(default)]
    pub queued: Option<i64>,
    #[serde(default)]
    pub running: Option<i64>,
    #[serde(default)]
    pub worktree_ids: Option<Vec<i64>>,
}

/// One row of `forge log --json`: a task as the queue lists it. Only
/// `finished_at` (null while queued or running), `project` (null for a
/// task predating projects) and `initiative` (null outside one) are
/// optional; every other field is always sent.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TaskRow {
    pub id: i64,
    pub state: String,
    pub workflow: String,
    pub attempts: i64,
    pub cost_usd: f64,
    pub repo: String,
    pub text: String,
    pub task: String,
    pub created_at: i64,
    pub created: String,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub initiative: Option<i64>,
    pub trust: String,
}

/// One row of `forge requests --json`: a blocked task and what it is
/// waiting on.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RequestRow {
    pub id: i64,
    pub kind: String,
    /// Who the question is addressed to; `None` means the operator.
    #[serde(default)]
    pub to: Option<String>,
    pub question: String,
    pub text: String,
    pub tried: String,
    pub path: String,
    pub workflow: String,
    pub repo: String,
    pub task: String,
}

/// One row of `forge plugin list --json`: a plugin as discovered.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PluginRow {
    pub name: String,
    pub description: String,
    pub dir: String,
    pub source: String,
    pub capabilities: Vec<String>,
    pub restart: String,
    pub enabled: bool,
}

/// One row of `forge plugin status --json`: whether a plugin is enabled
/// and, per the supervisor's last record, whether it is `running` (with
/// `pid`/`uptime_secs`), `restarting` (with `restart_count`), or `stopped`
/// (with `last_exit`).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PluginStatusRow {
    pub name: String,
    pub enabled: bool,
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
/// answer to a blocked task's question, or (`task_id: null`) a task-less
/// administrative decision such as `forge stats --reprice`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DecisionRow {
    pub id: i64,
    #[serde(default)]
    pub task_id: Option<i64>,
    pub repo: String,
    pub question: String,
    pub answer: String,
    pub created_at: i64,
    pub answered_by: String,
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
    pub repo: String,
    #[serde(default)]
    pub scope: Option<String>,
}

/// One row of `forge project list --json` / `forge project show --json`:
/// a project, the repositories it works in, task counts by state, cost,
/// and its own defaults. `workflow`, `per_task_usd`, `per_initiative_usd`,
/// `supervisor_model` and `supervisor_per_lineage` are the only optional
/// fields (each null falls to a wider default); every other field is
/// always sent.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProjectRow {
    pub name: String,
    pub purpose: String,
    pub created_at: i64,
    pub repos: Vec<ProjectRepoRow>,
    pub queued: i64,
    pub running: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub unverified: i64,
    pub blocked: i64,
    pub withdrawn: i64,
    pub cost_usd: f64,
    /// Jobs started in the last rolling 24h, counted separately from the
    /// task counts above (docs/JOBS.md step 1d).
    pub jobs_today: i64,
    pub jobs_ok: i64,
    pub jobs_failed: i64,
    pub jobs_needs_human: i64,
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
    pub protected: Vec<String>,
}

/// One row of `forge project backlog NAME --json`: a thing worth doing
/// that is not yet queued.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct BacklogRow {
    pub id: i64,
    pub project: String,
    pub text: String,
    pub created_at: i64,
    #[serde(default)]
    pub done_at: Option<i64>,
}

/// One deploy target on [`PortalDoc`]: the customer's "Running for you"
/// list (see docs/PORTAL.md, "What they see").
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalDeployTarget {
    pub name: String,
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
/// or `"needs_human"`, whether it was a dry run (a rehearsal), and every
/// effect it logged, each one sentence, never a kind or a target.
/// `reason`, on a failure or a needs-human run, is the human rung's
/// question in the customer's own terms; `None` on a needs-human run
/// means the question is addressed to the operator, not them — the
/// portal shows "we're on it" instead.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalJobRun {
    pub started_at: i64,
    pub state: String,
    pub dry_run: bool,
    pub effects: Vec<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// One run workflow on [`PortalDoc`]: an automation this project's jobs
/// run through, described in its own workflow file's words, and its last
/// three jobs, newest first.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalWorkflow {
    pub name: String,
    pub description: String,
    pub jobs: Vec<PortalJobRun>,
}

/// One open initiative on [`PortalDoc`]: the customer's "Being built"
/// list, newest first, capped at ten (`PortalDoc.initiatives_more` the
/// rest). `state` is always "in progress" or "waiting on you"; `pieces`
/// how many tasks make up the initiative so far.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalInitiative {
    pub outcome: String,
    pub state: String,
    pub pieces: i64,
}

/// One open question on [`PortalDoc`], addressed to the customer: the
/// "Needs you" list; `asked_at` is Unix seconds, when the task blocked on
/// it (`None` from a `forge` that predates the field).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalQuestion {
    pub task_id: i64,
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
    pub text: String,
    #[serde(default)]
    pub pieces: Option<i64>,
    pub landed_at: i64,
}

/// The confirmed intake brief on [`PortalDoc`] (see docs/INTAKE.md):
/// "Your plan".
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalBrief {
    pub where_it_runs: String,
    pub workflows: Vec<String>,
}

/// One open backlog item on [`PortalDoc`].
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalBacklogItem {
    pub id: i64,
    pub text: String,
    pub created_at: i64,
}

/// The document `forge project view NAME --json` prints: everything the
/// customer portal's page needs for one project, in their own words (see
/// docs/PORTAL.md, "What they see"). No branches, costs, attempt data or
/// verdict rows — those belong to the operator's page, not this one.
/// `brief` is the only optional field of this struct itself (null for a
/// project with no intake behind it); every other field is always sent —
/// though the nested rows it carries have their own optional fields, see
/// each one's own doc.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PortalDoc {
    pub project: String,
    /// The project's purpose; never rendered on the customer's page.
    pub purpose: String,
    pub deploy_targets: Vec<PortalDeployTarget>,
    pub run_workflows: Vec<PortalWorkflow>,
    pub initiatives: Vec<PortalInitiative>,
    /// How many open initiatives past the ten in `initiatives`.
    pub initiatives_more: i64,
    pub questions: Vec<PortalQuestion>,
    pub landed: Vec<PortalLanded>,
    /// How many landed lines past the ten in `landed`.
    pub landed_more: i64,
    #[serde(default)]
    pub brief: Option<PortalBrief>,
    pub backlog: Vec<PortalBacklogItem>,
}

/// One row of `forge initiative list --json` / `forge initiative show
/// --json`: an initiative, its derived state, task counts by state, cost
/// and its own settings. `held_rule`, `budget_usd` and `settled_at` are
/// the only optional fields; every other field is always sent.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitiativeRow {
    pub id: i64,
    pub project: String,
    pub outcome: String,
    pub state: String,
    #[serde(default)]
    pub held_rule: Option<String>,
    pub queued: i64,
    pub running: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub unverified: i64,
    pub blocked: i64,
    pub withdrawn: i64,
    pub cost_usd: f64,
    #[serde(default)]
    pub budget_usd: Option<f64>,
    pub stop_after_same_rule: i64,
    pub created_at: i64,
    #[serde(default)]
    pub settled_at: Option<i64>,
}

/// One lineage in [`InitiativeDoc::tasks`]: its latest task's id, state
/// and reason, plus how many retries the lineage took to reach it, its
/// own total cost across every attempt, and the assess directive's score
/// for its own landing, if any ran (see docs/ACTIONS.md, "Assessment").
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitiativeTaskRow {
    pub id: i64,
    pub state: String,
    pub reason: String,
    pub retries: i64,
    #[serde(default)]
    pub score: Option<i64>,
    pub cost_usd: f64,
}

/// One row of `InitiativeDoc.refused`: a verification rule name and how
/// many attempts of the initiative's tasks it refused.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RefusedRow {
    pub rule: String,
    pub count: i64,
}

/// One row of `InitiativeDoc.rulings`: a decision the supervisor made on
/// one of the initiative's tasks.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitiativeRulingRow {
    pub task_id: i64,
    pub question: String,
    pub answer: String,
    pub citations: String,
}

/// One row of `InitiativeDoc.questions`: a question that reached the
/// operator, answered or (while the task is still blocked) not yet.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitiativeQuestionRow {
    pub task_id: i64,
    pub question: String,
    #[serde(default)]
    pub answer: Option<String>,
}

/// One row of `InitiativeDoc.deployed`: a deploy one of the initiative's
/// tasks triggered on landing.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitiativeDeployRow {
    pub task_id: i64,
    pub target: String,
    pub sha: String,
    #[serde(default)]
    pub check_ok: Option<bool>,
    #[serde(default)]
    pub rolled_back_to: Option<String>,
}

/// The document `forge initiative report ID --json` prints: the outcome,
/// each task and how it ended, what verification refused, what the
/// supervisor ruled, what reached the operator, cost and elapsed time.
/// `held_rule`, `budget_usd`, `elapsed_secs` and `settled_at` are the
/// only optional fields; every other field is always sent.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitiativeDoc {
    pub id: i64,
    pub project: String,
    pub outcome: String,
    pub state: String,
    #[serde(default)]
    pub held_rule: Option<String>,
    #[serde(default)]
    pub budget_usd: Option<f64>,
    pub stop_after_same_rule: i64,
    pub tasks: Vec<InitiativeTaskRow>,
    pub refused: Vec<RefusedRow>,
    pub rulings: Vec<InitiativeRulingRow>,
    pub questions: Vec<InitiativeQuestionRow>,
    pub deployed: Vec<InitiativeDeployRow>,
    pub cost_usd: f64,
    #[serde(default)]
    pub elapsed_secs: Option<i64>,
    pub created_at: i64,
    #[serde(default)]
    pub settled_at: Option<i64>,
}

/// One row of `forge job list --json`: a job, one run of a `kind = "run"`
/// workflow (see docs/JOBS.md). `finished_at`, `cost_usd` and `due_at`
/// are the only optional fields; every other field is always sent.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct JobRow {
    pub id: i64,
    pub project: String,
    pub workflow: String,
    pub workflow_hash: String,
    pub landed_sha: String,
    pub trigger_kind: String,
    pub trigger_ref: String,
    pub state: String,
    /// `"repo"` or `"catalog"`: where the workflow was resolved from (see
    /// docs/JOBS.md, "Where an automation lives").
    pub workflow_source: String,
    pub dry_run: bool,
    pub started_at: i64,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    pub verdict_json: String,
    /// When this job becomes claimable, a unix second; `None` for a job
    /// that was never delayed (see docs/JOBS.md, "Delayed jobs").
    #[serde(default)]
    pub due_at: Option<i64>,
}

/// One row of `JobDoc.steps`: one step of a job's run. `cost_usd`,
/// `finished_at` and `exit_code` are the only optional fields (an
/// in-flight or directive step has none of them yet); every other field
/// is always sent.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct JobStepRow {
    pub id: i64,
    pub job_id: i64,
    pub seq: i64,
    pub action: String,
    pub kind: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    pub started_at: i64,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub output_ref: String,
    pub tail: String,
}

/// One row of `JobDoc.effects` and of `forge job log --json`: one effect a
/// job's step performed on the world.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct JobEffectRow {
    pub id: i64,
    pub job_id: i64,
    pub seq: i64,
    pub kind: String,
    pub target: String,
    pub summary: String,
    pub dry_run: bool,
}

/// The document `forge job show ID --json` prints: one job with every step
/// and effect it recorded. `finished_at`, `cost_usd` and `due_at` are the
/// only optional fields; every other field is always sent.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct JobDoc {
    pub id: i64,
    pub project: String,
    pub workflow: String,
    pub workflow_hash: String,
    pub landed_sha: String,
    pub trigger_kind: String,
    pub trigger_ref: String,
    pub state: String,
    /// `"repo"` or `"catalog"`: where the workflow was resolved from (see
    /// docs/JOBS.md, "Where an automation lives").
    pub workflow_source: String,
    pub dry_run: bool,
    pub started_at: i64,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    pub verdict_json: String,
    /// When this job becomes claimable, a unix second; `None` for a job
    /// that was never delayed (see docs/JOBS.md, "Delayed jobs").
    #[serde(default)]
    pub due_at: Option<i64>,
    pub steps: Vec<JobStepRow>,
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
    pub attempt_no: i64,
    pub step: String,
    pub step_seq: i64,
    pub state: String,
    pub reason: String,
    pub started_at: i64,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub agent_exit: Option<i32>,
    pub timed_out: bool,
    pub num_turns: i64,
    pub tool_calls: i64,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    pub agent_ms: i64,
    pub commits: i64,
    pub files_changed: i64,
    pub dirty: bool,
    pub start_sha: String,
    pub end_sha: String,
    pub log_path: String,
    pub tokens: Tokens,
    pub rate_limits: RateLimits,
    pub inputs: Value,
    pub outputs: Value,
    pub verdict: Value,
    pub envelope: Value,
}

/// One row of `forge project deploy list <project> --json`: a deploy
/// target as declared (see docs/DEPLOY.md, "A target"). `scope` and
/// `smoke_url` are the only optional fields; every other field is always
/// sent.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DeployTargetRow {
    pub project: String,
    pub name: String,
    pub repo: String,
    #[serde(default)]
    pub scope: Option<String>,
    pub method: String,
    pub args: BTreeMap<String, String>,
    pub check_cmd: String,
    pub on_landing: bool,
    #[serde(default)]
    pub smoke_url: Option<String>,
}

/// One entry of `TraceDoc.deploys`: one deploy the task triggered on
/// landing. `finished_at`, `check_ok`, `rolled_back_to`, `smoke_ok`,
/// `smoke_json`, `look_ok` and `look_json` are the only optional fields
/// (each set only once its own step has run); every other field is
/// always sent.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Deploy {
    pub id: i64,
    pub project: String,
    pub target: String,
    pub sha: String,
    pub started_at: i64,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub check_ok: Option<bool>,
    pub check_output: String,
    #[serde(default)]
    pub rolled_back_to: Option<String>,
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
    pub path: String,
    pub finding: String,
    pub severity: String,
}

/// `TraceDoc.assessment`: the assess directive's most recent run against
/// this task's own landing (see docs/ACTIONS.md, "Assessment"). Absent
/// when it never ran.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Assessment {
    pub score: i64,
    pub findings: Vec<Finding>,
    pub model: String,
    pub provider: String,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    pub created_at: i64,
}

/// One entry of `TraceDoc.ops`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Op {
    pub id: i64,
    pub seq: i64,
    pub name: String,
    pub kernel: bool,
    pub started_at: i64,
    pub ms: i64,
    pub ok: bool,
    #[serde(default)]
    pub exit: Option<i32>,
    pub detail: String,
    #[serde(default)]
    pub attempt_id: Option<i64>,
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
    pub task: Value,
    pub attempts: Vec<Attempt>,
    pub ops: Vec<Op>,
    pub resolved: Value,
    pub diagnosis: Value,
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
    pub role: String,
    pub provider: String,
    pub model: String,
    pub kind: String,
    pub attempts: i64,
    pub succeeded: i64,
    #[serde(default)]
    pub succeeded_share: Option<f64>,
    pub mean_turns: f64,
    pub mean_cost_usd: f64,
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
    pub factor: String,
    pub level: String,
    pub tasks: i64,
    pub landed: i64,
    pub rate: f64,
    pub rate_lo: f64,
    pub rate_hi: f64,
    #[serde(default)]
    pub mean_true_cost_usd: Option<f64>,
    pub is_reference: bool,
    #[serde(default)]
    pub effect: Option<f64>,
    #[serde(default)]
    pub effect_se: Option<f64>,
    #[serde(default)]
    pub mean_grep_then_ranged_read_chains: Option<f64>,
    #[serde(default)]
    pub mean_unedited_read_chars: Option<f64>,
    #[serde(default)]
    pub mean_turns_before_first_edit: Option<f64>,
    #[serde(default)]
    pub mean_outline_calls: Option<f64>,
    #[serde(default)]
    pub mean_def_calls: Option<f64>,
}

/// The document `forge stats --json` prints. `docs/CLIENT.md` documents
/// the full shape (`workflows`, `steps`, `journal`, `no_journal`,
/// `projects`, `by_role`, `factors`, `tools`); only `by_role` and
/// `factors` are modeled here today, the rest added as a client needs
/// them.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct StatsDoc {
    pub by_role: Vec<StatsRoleRow>,
    pub factors: Vec<StatsFactorRow>,
}

/// The document `forge graph REPO --json` prints: the module graph
/// (docs/LATER.md, "The code visualiser") for a repository at its
/// working tree, with each file node's overlay from the record laid on
/// top (docs/LATER.md, "The overlay, from the record").
#[derive(Debug, Clone, Deserialize, Default)]
pub struct GraphDoc {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// One node of `GraphDoc`: a source file `forge-repomap` extracts, or a
/// directory that groups files directly under it (`kind` is `"file"` or
/// `"module"`). `overlay` is only ever populated for a `"file"` node.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct GraphNode {
    pub path: String,
    pub kind: String,
    pub symbols: i64,
    pub lines: i64,
    pub overlay: Overlay,
}

/// One edge of `GraphDoc`: an import that resolves to another file in
/// the tree.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
}

/// What the record knows about one file node (`GraphNode::overlay`):
/// empty for a file no task ever touched.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Overlay {
    pub tasks: Vec<TaskTouch>,
    pub demotions: Vec<Demotion>,
    pub repair_cost_usd: f64,
}

/// One task that touched a file node, from its attempts' recorded
/// `changes`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TaskTouch {
    pub id: i64,
    pub at: i64,
    pub cost_usd: f64,
}

/// One review demotion whose evidence names a file node.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Demotion {
    pub id: i64,
    pub at: i64,
    pub reason: String,
}

/// One workflow's declared metadata and measured outcomes, one entry of
/// `forge workflows --json`'s `"workflows"` array. Its shape is informal
/// (see `docs/CLIENT.md`: "no client reads it today"), so the nested
/// pieces stay raw JSON.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Workflow {
    pub name: String,
    /// `"build"` or `"run"`.
    pub kind: String,
    /// `"catalog"` (the operator's own) or `"repo"` (a project's own
    /// `.forge/workflows/`).
    pub source: String,
    pub hash: String,
    pub description: String,
    pub path: String,
    pub steps: Value,
    pub resolved: Value,
    pub meta: Value,
    pub measured: Value,
}

/// One resolved step of `forge workflows show --json`'s `"steps"` array:
/// the action's name, kind, contract, model, turns, timeout and
/// description, in the order the workflow runs them.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkflowStepDoc {
    pub name: String,
    pub kind: String,
    pub contract: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub timeout_secs: Option<u32>,
    pub description: String,
}

/// The document `forge workflows show NAME --json` prints: one workflow in
/// full. `measured` stays raw JSON, the same informal shape as
/// [`Workflow::measured`].
#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkflowShowDoc {
    pub name: String,
    /// `"catalog"` (the operator's own) or `"repo"` (a project's own
    /// `.forge/workflows/`).
    pub source: String,
    pub path: String,
    /// `"build"` or `"run"`.
    pub kind: String,
    /// The workflow file's exact text.
    pub text: String,
    pub steps: Vec<WorkflowStepDoc>,
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
