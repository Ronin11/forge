//! Spawn the agent CLI in the worktree and read its stream-json output
//! under a wall-clock timeout. The raw stream is the attempt's log, prompt
//! first. Numbers Forge records come from the CLI's accounting or Forge's
//! own clock, never from the model's prose.

pub(crate) mod build_env;
mod chat;
mod claude;
mod codex;
mod copilot;
mod inputs;
mod jev;
pub mod refusal;
mod relaunch;
mod usage_limit;
use crate::sandbox::Phase;
use chat::run_chat;
pub(super) use chat::truncated_first_line;
use claude::{apply_claude_result, claude_argv, run_claude};
use codex::{apply_codex_event, run_codex};
use copilot::{CopilotTally, apply_copilot_event, run_copilot};
use inputs::{AgentRun, RunCodexPhase, RunCopilotPhase, RunJsonPhase};
pub use jev::*;
pub(crate) use relaunch::Relaunch;
#[cfg(test)]
pub(crate) use relaunch::{fake_bwrap, launches};
pub use usage_limit::usage_limit_reset;

use crate::executor::Execution;
use crate::report::{Event, Reporter};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[derive(Default, Debug)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub got_result: bool,
    pub is_error: bool,
    pub num_turns: i64,
    pub tool_calls: i64,
    pub cost_usd: Option<f64>,
    /// The claude CLI's own `total_cost_usd`, kept when `cost_usd` was
    /// computed from tokens at the provider's list prices (`pricing`).
    pub cli_cost_usd: Option<f64>,
    /// Token counts from the result frame's usage object.
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub wall_ms: u128,
    pub result_text: String,
    /// The structured result the CLI produced against the envelope schema,
    /// as raw JSON; `None` when the result frame carried none.
    pub structured: Option<String>,
    /// Last rate-limit sample seen on the stream, per window.
    pub rate_limits: RateLimits,
    /// The CLI session, so a capped attempt can be resumed where it stopped.
    pub session_id: Option<String>,
    /// Forge ended the run itself because enough signs of an attempt going
    /// nowhere tripped (see `Watch`): the signs, for the record and the
    /// continuation prompt.
    pub ended_early: Option<String>,
    /// Which signs tripped: "no-edit", "uncommitted", "repeat". Recorded
    /// whether or not they ended the run, so the thresholds can be tuned
    /// from the store.
    pub early_signals: Vec<&'static str>,
    /// Which signs were within 20% of tripping when the run ended, and did
    /// not: the same tuning signal for thresholds that were almost right.
    pub early_near: Vec<&'static str>,
    /// The CLI ended the run at its turn limit.
    pub max_turns_hit: bool,
    /// The provider refused the run for a rate window: not the agent's fault.
    pub rate_limited: bool,
    /// The refusal was of the login itself: held until a probe answers.
    pub login_refused: bool,
    /// The result frame's own `subtype` (e.g. `"success"`,
    /// `"error_max_turns"`, `"error_during_execution"`); `None` when no
    /// result frame ever arrived. A directive job step quotes this in its
    /// failure tail instead of the bare exit code.
    pub subtype: Option<String>,
    /// The CLI’s terminal diagnosis, including exhausted structured-output retries.
    pub terminal_reason: Option<String>,
    /// Everything the agent wrote to stderr across the run. A directive job
    /// step's failure tail quotes the last lines of this.
    pub stderr_text: String,
}

/// Subscription usage as the CLI reports it: utilization is 0..1 of the
/// window, resets_at is unix seconds. On subscription billing this, not
/// the notional dollar figure, is the real budget.
#[derive(Default, Debug, Clone, Copy)]
pub struct RateLimits {
    pub five_hour: Option<(f64, i64)>,
    pub seven_day: Option<(f64, i64)>,
}

/// Which backend runs a provider: the three agent CLIs Forge knows how to
/// build a launch's argv and env for and parse the event stream of, plus a
/// fourth that spawns no CLI at all — one HTTP call to an OpenAI-compatible
/// `/chat/completions` endpoint, the whole of a job's directive step
/// (docs/JOBS.md, "Steps"; see `run_chat`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Runner {
    #[default]
    ClaudeCli,
    CodexCli,
    /// GitHub Copilot CLI, `copilot -p`; see `run_copilot`.
    CopilotCli,
    Chat,
    /// TypeSafe's Jev: typed judgment, one HTTP call, never text; see
    /// `jev::run`.
    Jev,
}

impl Runner {
    pub fn as_str(self) -> &'static str {
        match self {
            Runner::ClaudeCli => "claude-cli",
            Runner::CodexCli => "codex-cli",
            Runner::CopilotCli => "copilot-cli",
            Runner::Chat => "chat",
            Runner::Jev => "jev",
        }
    }
}

impl std::str::FromStr for Runner {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "claude-cli" => Ok(Runner::ClaudeCli),
            "codex-cli" => Ok(Runner::CodexCli),
            "copilot-cli" => Ok(Runner::CopilotCli),
            "chat" => Ok(Runner::Chat),
            "jev" => Ok(Runner::Jev),
            other => Err(format!(
                "unknown runner {other:?}; expected \"claude-cli\", \"codex-cli\", \"copilot-cli\", \"chat\" or \"jev\""
            )),
        }
    }
}

/// An agent backend as the operator's config names it under
/// `[providers.<name>]` (see `config::load_home`). The built-in "anthropic"
/// provider (`Provider::default`) needs no entry in the config to keep
/// today's behavior unchanged.
#[derive(Clone, Debug)]
pub struct Provider {
    pub name: String,
    pub runner: Runner,
    /// The model a task gets when neither it nor its workflow step says
    /// one; `None` leaves it to the CLI's own default (codex, signed into
    /// a plan rather than billed per token).
    pub model: Option<String>,
    pub base_url: Option<String>,
    /// The environment variable that holds this provider's API key, for
    /// `Runner::Chat`'s `Authorization` header — never the key itself,
    /// which stays out of the config file and the record (see
    /// `run_chat`). `None` for a provider that needs none (a local model).
    pub api_key_env: Option<String>,
    /// Names the variable holding a jev provider's Cloudflare account id.
    pub account_id_env: Option<String>,
    /// A jev provider's host (`jev::JevBackend`) and its Cloudflare keys.
    pub jev_backend: JevBackend,
    pub cloudflare_url: Option<String>,
    pub cloudflare_key_env: Option<String>,
    pub cloudflare_model: Option<String>,
    pub env: Vec<(String, String)>,
    /// Extra argv this provider always adds, after the launcher's own
    /// flags and before the prompt (e.g. codex's `--oss --local-provider
    /// ollama` for a local model).
    pub extra_args: Vec<String>,
    pub notes: Option<String>,
    /// USD per million tokens, for a runner whose CLI reports no cost of
    /// its own (codex): 0 for a local model.
    pub price_input_per_million: f64,
    pub price_output_per_million: f64,
    /// USD per million cache-read tokens for a priced claude provider;
    /// `None` means a tenth of the input price.
    pub price_cache_read_per_million: Option<f64>,
    /// USD per premium request, for a runner whose CLI meters requests
    /// rather than tokens (copilot: a plan's monthly allowance, then a list
    /// price per request over it); 0 within the allowance. Never consulted
    /// by the other runners.
    pub price_per_request: f64,
    /// This provider's own rate-window caps (fractions of the window), as
    /// `[providers.<name>]` may override; default to the operator's
    /// `[budget]` caps when it does not (see `config::build_providers`).
    /// The worker holds this provider alone once its own latest sample
    /// reaches its own cap (see `worker::window_hold`).
    pub five_hour_max: f64,
    pub seven_day_max: f64,
    /// How many times `run_codex` may nudge a phase one that made no
    /// progress (no edit, or an edit left uncommitted) before it runs phase
    /// two: a fixed, schema-free prompt resuming the same thread, told to
    /// do the work and commit rather than describe it (dev.home's
    /// qwen3-coder:30b, tasks 309/310/313). 0 (the default) leaves today's
    /// behavior unchanged; never consulted for `Runner::ClaudeCli`.
    pub nudges: u32,
}

impl Default for Provider {
    fn default() -> Self {
        Provider {
            name: "anthropic".into(),
            runner: Runner::ClaudeCli,
            model: Some("sonnet".into()),
            base_url: None,
            api_key_env: None,
            account_id_env: None,
            jev_backend: JevBackend::Auto,
            cloudflare_url: None,
            cloudflare_key_env: None,
            cloudflare_model: None,
            env: Vec::new(),
            extra_args: Vec::new(),
            notes: None,
            price_input_per_million: 0.0,
            price_output_per_million: 0.0,
            price_cache_read_per_million: None,
            price_per_request: 0.0,
            five_hour_max: 0.9,
            seven_day_max: 0.95,
            nudges: 0,
        }
    }
}

pub fn agent_bin() -> String {
    crate::config::env("CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string())
}

/// The codex CLI, `FORGE_CODEX_BIN` overridden (the codex-cli fake in
/// tests, an alternate build on an operator's machine).
pub fn codex_bin() -> String {
    crate::config::env("CODEX_BIN").unwrap_or_else(|_| "codex".to_string())
}

/// `codex_bin`'s per-step override, `FORGE_CODEX_BIN_<STEP>`; see
/// `agent_bin_for`, which does the same for the claude CLI.
pub fn codex_bin_for(step: &str) -> String {
    let suffix = format!("CODEX_BIN_{}", step.to_ascii_uppercase().replace('-', "_"));
    crate::config::env(&suffix).unwrap_or_else(|_| codex_bin())
}

/// The copilot CLI, `FORGE_COPILOT_BIN` overridden (the copilot-cli fake in
/// tests, an alternate build on an operator's machine).
pub fn copilot_bin() -> String {
    crate::config::env("COPILOT_BIN").unwrap_or_else(|_| "copilot".to_string())
}

/// `copilot_bin`'s per-step override, `FORGE_COPILOT_BIN_<STEP>`; see
/// `agent_bin_for`.
pub fn copilot_bin_for(step: &str) -> String {
    let suffix = format!(
        "COPILOT_BIN_{}",
        step.to_ascii_uppercase().replace('-', "_")
    );
    crate::config::env(&suffix).unwrap_or_else(|_| copilot_bin())
}

/// The binary behind a bare name, past any version-manager shim or wrapper:
/// `mise which` on the host when mise manages it, else the canonical path
/// on PATH. A path given explicitly is used as is.
pub fn real_bin(name: &str) -> String {
    if name.contains('/') {
        return name.to_string();
    }
    if let Ok(out) = std::process::Command::new("mise")
        .args(["which", name])
        .output()
        && out.status.success()
    {
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !p.is_empty() && std::path::Path::new(&p).exists() {
            return p;
        }
    }
    crate::sandbox::resolve_binary(name)
        .map(|(_, canonical)| canonical.display().to_string())
        .unwrap_or_else(|_| name.to_string())
}

/// A step may run a different agent binary through FORGE_CLAUDE_BIN_<STEP>
/// (upper-cased action name), which is how the test suite plays every
/// role in a workflow with a different script.
pub fn agent_bin_for(step: &str) -> String {
    let suffix = format!("CLAUDE_BIN_{}", step.to_ascii_uppercase().replace('-', "_"));
    crate::config::env(&suffix).unwrap_or_else(|_| agent_bin())
}

/// `PATH` with the directory of this binary in front, where Forge's own
/// tools (`forge-repomap`) live; `Forge::open` binds that directory into
/// the sandbox, so a name on this `PATH` runs inside it too.
fn path_with_bin_dir(path: &str) -> String {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.display().to_string()));
    match dir {
        Some(d) if !path.split(':').any(|p| p == d) => format!("{d}:{path}"),
        _ => path.to_string(),
    }
}

/// Which inherited variables a launch may receive. Explicit check
/// configuration is appended separately, after this filter.
fn inherited_env_allowed(key: &str, phase: Phase) -> bool {
    matches!(
        key,
        "PATH" | "HOME" | "LANG" | "TERM" | "FAKE_SLEEP" | "FAKE_SLEEP_SECS"
    ) || key.starts_with("LC_")
        || (phase == Phase::Agent
            && (key == "CLAUDE_CONFIG_DIR"
                || ["ANTHROPIC_", "CODEX_", "COPILOT_"]
                    .iter()
                    .any(|prefix| key.starts_with(prefix))))
}

/// The inherited environment for this phase, sandboxed or not. The
/// sandbox overrides HOME on top of it.
pub fn agent_env(phase: Phase) -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(key, _)| inherited_env_allowed(key, phase))
        .map(|(k, v)| {
            if k == "PATH" {
                (k, path_with_bin_dir(&v))
            } else {
                (k, v)
            }
        })
        .collect()
}

/// The phase's inherited environment plus explicit values and build limits.
fn env_with(
    worktree: &Path,
    extra_env: &[(String, String)],
    phase: Phase,
) -> Vec<(String, String)> {
    let mut env = agent_env(phase);
    env.extend(extra_env.iter().cloned());
    env.extend(build_env::worktree_env(worktree));
    env
}

/// What must happen before `command_in` builds a command for the same
/// arguments: a sandbox seeds its private logins, waiting on their locks
/// without holding a thread (see `Sandbox::prepare`).
pub async fn prepare_in(
    sandbox: Option<&Execution>,
    worktree: &Path,
    extra_env: &[(String, String)],
    phase: Phase,
) {
    if let Some(sb) = sandbox {
        sb.prepare(worktree, &env_with(worktree, extra_env, phase), phase)
            .await;
    }
}

/// A command for `argv` in the worktree, through the sandbox when there is
/// one, with the agent environment plus `extra_env`. `prepare_in` is
/// awaited first.
pub fn command_in(
    sandbox: Option<&Execution>,
    worktree: &Path,
    argv: &[String],
    extra_env: &[(String, String)],
    phase: Phase,
) -> std::process::Command {
    let env = env_with(worktree, extra_env, phase);
    match sandbox {
        Some(sb) => sb.command(worktree, argv, &env, phase),
        None => crate::executor::Executor::command(
            &crate::executor::Host,
            worktree,
            argv,
            &env,
            &crate::egress::Policy::new([]),
            phase,
        ),
    }
}

/// Spawns the command `make` builds, retrying briefly on `ETXTBSY`. A
/// script just written and chmod'd can still read as busy for a few
/// milliseconds after the writer closes it — a kernel race distinct from
/// the bwrap bind-mount one `run_with_relaunch` retries, since this one
/// never gets as far as a child process; there is nothing to relaunch,
/// only the spawn to redo. `make` is called again on each attempt because
/// a `Command` is consumed by `spawn`.
async fn spawn_retrying_etxtbsy(
    mut make: impl FnMut() -> tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    const MAX_ATTEMPTS: u32 = 20;
    for attempt in 1..=MAX_ATTEMPTS {
        match make().spawn() {
            Err(e) if attempt < MAX_ATTEMPTS && e.raw_os_error() == Some(libc::ETXTBSY) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            result => return result,
        }
    }
    unreachable!()
}

pub struct Launch<'a> {
    pub task_id: i64,
    pub worktree: &'a Path,
    /// Resolved from trusted repository metadata before entering the runner.
    pub identity: Vec<(String, String)>,
    pub prompt: &'a str,
    /// System-level content a runner with its own system channel
    /// (`Runner::Chat`) sends as a separate message ahead of `prompt`;
    /// every other runner takes one prompt and ignores this. Empty when a
    /// caller has none of its own (`Runner::Chat` then falls back to the
    /// untrusted-data sentence alone; see `run_chat`).
    pub system: &'a str,
    pub model: &'a str,
    pub max_turns: u32,
    pub timeout: Duration,
    /// How long the checks after this run may take (zero when none follow):
    /// the login must outlast both (see `login::refresh_window_ms`).
    pub check_timeout: Duration,
    pub log_path: &'a Path,
    pub sandbox: Option<&'a Execution>,
    pub report: &'a Reporter,
    pub step: &'a str,
    /// The provider this step runs under: which CLI, and what it adds to
    /// the launch's argv and env.
    pub provider: &'a Provider,
    /// A CLI session to continue instead of starting fresh.
    pub resume: Option<&'a str>,
    /// The attempt's own starting commit — the original attempt's, across a
    /// resume, since the agent's report covers the whole session (see
    /// `attempt::new_attempt`). `run_codex`'s nudge loop compares the
    /// worktree against this to tell "no edit at all" from "edited, not
    /// committed".
    pub start_sha: &'a str,
    /// Whether this step is expected to change files (code, tests). A
    /// read-only step (review, plan) is never faulted for not editing.
    pub writes: bool,
    /// The JSON schema the CLI holds the structured result to; the
    /// envelope for every directive, the supervisor's own for it.
    pub schema: &'a str,
    /// Thresholds for `Watch`, the operator's `[early_ending]` config.
    pub early_ending: crate::config::EarlyEnding,
    /// A job's directive step (docs/JOBS.md, "Steps"): the agent runs with
    /// no tools at all. The claude backend passes the flag that disables
    /// every tool; the codex backend cannot yet guarantee the same and
    /// refuses the run instead of pretending to (see `run`).
    pub no_tools: bool,
    /// What a `Runner::Jev` judges; `None` outside a job's directive step.
    pub judgment: Option<Judgment<'a>>,
}

/// Live signs that an attempt is going nowhere, computed from the tool
/// calls as they stream. `signals_to_end` of them together end the run:
/// the session is kept and resumed with a prompt that names them, which is
/// cheaper than letting the cap arrive. Default thresholds come from the
/// first 214 attempts, where capped coders had made no edit by call 30 and
/// the ones that did edit were committing every few edits; the operator's
/// `[early_ending]` config can override them (see `src/config.rs`).
struct Watch {
    thresholds: crate::config::EarlyEnding,
    calls: u32,
    edits: u32,
    edits_since_commit: u32,
    commands: std::collections::HashMap<String, u32>,
}

/// Whether `value` sits in the top 20% below `threshold`, without having
/// reached it: close enough to call a near miss. A threshold of 0 has no
/// "near" band, only tripped or not.
fn is_near(value: u32, threshold: u32) -> bool {
    threshold > 0 && value < threshold && (value as f64) >= (threshold as f64) * 0.8
}

impl Watch {
    fn new(thresholds: crate::config::EarlyEnding) -> Self {
        Watch {
            thresholds,
            calls: 0,
            edits: 0,
            edits_since_commit: 0,
            commands: std::collections::HashMap::new(),
        }
    }

    fn saw(&mut self, name: &str, input: &Value) {
        self.calls += 1;
        match name {
            "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
                self.edits += 1;
                self.edits_since_commit += 1;
            }
            "Bash" => {
                let cmd = input["command"].as_str().unwrap_or("").trim().to_string();
                if cmd.contains("git commit") {
                    self.edits_since_commit = 0;
                }
                if !cmd.is_empty() {
                    *self.commands.entry(cmd).or_insert(0) += 1;
                }
            }
            _ => {}
        }
    }

    fn max_repeat(&self) -> Option<(&str, u32)> {
        self.commands
            .iter()
            .max_by_key(|(_, n)| **n)
            .map(|(cmd, n)| (cmd.as_str(), *n))
    }

    /// The signs that have tripped: (kind, what happened).
    fn tripped(&self, writes: bool) -> Vec<(&'static str, String)> {
        let t = &self.thresholds;
        let mut out = Vec::new();
        if writes && self.calls >= t.no_edit_calls && self.edits == 0 {
            out.push(("no-edit", format!("{} tool calls with no edit", self.calls)));
        }
        if writes && self.edits_since_commit >= t.edits_without_commit {
            out.push((
                "uncommitted",
                format!("{} edits since the last commit", self.edits_since_commit),
            ));
        }
        if let Some((cmd, n)) = self.max_repeat()
            && n >= t.repeats
        {
            let short: String = cmd.chars().take(60).collect();
            out.push(("repeat", format!("`{short}` run {n} times")));
        }
        out
    }

    /// Signs within 20% of tripping but that have not: the same signals,
    /// so the thresholds can be tuned from attempts that did not trip them.
    fn near(&self, writes: bool) -> Vec<&'static str> {
        let t = &self.thresholds;
        let mut out = Vec::new();
        if writes && self.edits == 0 && is_near(self.calls, t.no_edit_calls) {
            out.push("no-edit");
        }
        if writes && is_near(self.edits_since_commit, t.edits_without_commit) {
            out.push("uncommitted");
        }
        if let Some((_, n)) = self.max_repeat()
            && is_near(n, t.repeats)
        {
            out.push("repeat");
        }
        out
    }

    /// The signs that have tripped, when there are enough of them to end
    /// the run; `None` when `signals_to_end` is 0 (early ending disabled)
    /// or too few have tripped yet.
    fn should_end(&self, writes: bool) -> Option<Vec<(&'static str, String)>> {
        let n = self.thresholds.signals_to_end;
        if n == 0 {
            return None;
        }
        let tripped = self.tripped(writes);
        (tripped.len() >= n as usize).then_some(tripped)
    }
}

/// One spawn of `argv` through to exit or timeout: the same shape `run` has
/// always had, just factored out so `run` can retry it on the bwrap
/// bind-mount race without re-deriving `bin`/`argv` from `Launch` (which
/// would need a live `claude` binary in tests) and without disturbing the
/// per-line `forge_ms` timestamps, which must measure from this attempt's
/// own spawn, not from whenever a caller-side retry loop happens to notice
/// it finished.
async fn run_once(args: AgentRun<'_>) -> Result<(Outcome, String)> {
    let AgentRun {
        sandbox,
        worktree,
        argv,
        identity,
        prompt,
        bin,
        timeout,
        writes,
        early_ending,
        task_id,
        report,
        log,
    } = args;
    prepare_in(sandbox, worktree, identity, Phase::Agent).await;
    let mut child = spawn_retrying_etxtbsy(|| {
        let mut c = Command::from(command_in(sandbox, worktree, argv, identity, Phase::Agent));
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        c
    })
    .await
    .with_context(|| format!("spawning {bin}"))?;

    {
        let mut stdin = child.stdin.take().context("agent stdin")?;
        // A transient bwrap failure closes this pipe before the write
        // lands; that must not fail the launch outright.
        let _ = stdin.write_all(prompt.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }
    let stderr = child.stderr.take().context("agent stderr")?;
    let stderr_task = tokio::spawn(async move {
        let mut s = String::new();
        BufReader::new(stderr).read_to_string(&mut s).await.ok();
        s
    });

    let start = Instant::now();
    let deadline = tokio::time::Instant::now() + timeout;
    let mut out = Outcome::default();
    let mut seen_tools: HashSet<String> = HashSet::new();
    let mut watch = Watch::new(early_ending);
    let stdout = child.stdout.take().context("agent stdout")?;
    let mut lines = BufReader::new(stdout).lines();

    let read = async {
        while let Some(line) = lines.next_line().await? {
            // The CLI's frames carry no clock; Forge stamps each with its
            // own, so a tool call and its result measure a duration.
            let Ok(mut v) = serde_json::from_str::<Value>(&line) else {
                writeln!(log, "{line}")?;
                continue;
            };
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "forge_ms".into(),
                    Value::from(start.elapsed().as_millis() as u64),
                );
            }
            writeln!(log, "{v}")?;
            match v["type"].as_str() {
                Some("assistant") => {
                    // The CLI repeats a message once per content block; count
                    // each tool_use id once.
                    if let Some(blocks) = v["message"]["content"].as_array() {
                        for b in blocks {
                            if b["type"] == "tool_use" {
                                let id = b["id"].as_str().unwrap_or("").to_string();
                                if seen_tools.insert(id) {
                                    out.tool_calls += 1;
                                    let name = b["name"].as_str().unwrap_or("?");
                                    report.emit(task_id, Event::ToolCall { name });
                                    watch.saw(name, &b["input"]);
                                }
                            }
                        }
                    }
                    if let Some(tripped) = watch.should_end(writes) {
                        let text = tripped
                            .iter()
                            .map(|(_, w)| w.as_str())
                            .collect::<Vec<_>>()
                            .join("; ");
                        writeln!(
                            log,
                            "{{\"type\":\"forge_early_end\",\"forge_ms\":{},\"signals\":{}}}",
                            start.elapsed().as_millis(),
                            serde_json::to_string(&text)?
                        )?;
                        report.emit(
                            task_id,
                            Event::Note {
                                text: &format!("early    stopped: {text}"),
                            },
                        );
                        out.ended_early = Some(text);
                        break;
                    }
                }
                Some("system") => {
                    if let Some(id) = v["session_id"].as_str() {
                        out.session_id = Some(id.to_string());
                    }
                }
                Some("result") => {
                    apply_claude_result(&mut out, &v);
                }
                Some("rate_limit_event") => {
                    let w = &v["rate_limit_info"]["unifiedWindows"];
                    let read = |name: &str| {
                        let win = &w[name];
                        Some((
                            win["utilization"].as_f64()?,
                            win["resetsAt"].as_i64().unwrap_or(0),
                        ))
                    };
                    if let Some(s) = read("five_hour") {
                        out.rate_limits.five_hour = Some(s);
                    }
                    if let Some(s) = read("seven_day") {
                        out.rate_limits.seven_day = Some(s);
                    }
                    // A refused run: hold the shorter window until the reset
                    // the provider named (or five minutes), whichever the
                    // sample does not already say.
                    if v["rate_limit_info"]["status"].as_str() == Some("rejected") {
                        out.rate_limited = true;
                        let resets = v["rate_limit_info"]["resetsAt"]
                            .as_i64()
                            .unwrap_or_else(|| crate::unix_now() + 300);
                        let (u, r) = out.rate_limits.five_hour.unwrap_or((1.0, resets));
                        out.rate_limits.five_hour =
                            Some((u.max(1.0), if r > 0 { r } else { resets }));
                    }
                }
                _ => {}
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    match tokio::time::timeout_at(deadline, read).await {
        Ok(r) => {
            r?;
            if out.ended_early.is_some() {
                child.kill().await.ok();
                child.wait().await.ok();
            } else {
                match tokio::time::timeout_at(deadline, child.wait()).await {
                    Ok(status) => out.exit_code = status?.code(),
                    Err(_) => out.timed_out = true,
                }
            }
        }
        Err(_) => out.timed_out = true,
    }
    out.early_signals = watch.tripped(writes).iter().map(|(k, _)| *k).collect();
    out.early_near = watch.near(writes);
    if out.timed_out {
        child.kill().await.ok();
        child.wait().await.ok();
        writeln!(
            log,
            "{{\"type\":\"forge_timeout\",\"after_secs\":{}}}",
            timeout.as_secs()
        )?;
    }
    out.wall_ms = start.elapsed().as_millis();

    let stderr_text = stderr_task.await.unwrap_or_default();
    Ok((out, stderr_text))
}

/// Runs `run_once`, retrying up to three times when bwrap loses the
/// seed-bind race documented on `relaunch::is_transient_bwrap_failure`: the child
/// exits within two seconds with that message on stderr. That is a launch
/// failure, not an attempt, so it gets a few silent relaunches rather than
/// burning one of the attempt's own retries. Any other quick exit (a real
/// crash, a fast fake in tests) is returned as is.
async fn run_with_relaunch(args: AgentRun<'_>) -> Result<(Outcome, String)> {
    let AgentRun {
        sandbox,
        worktree,
        argv,
        identity,
        prompt,
        bin,
        timeout,
        writes,
        early_ending,
        task_id,
        report,
        log,
    } = args;
    let mut relaunch = Relaunch::default();
    loop {
        let (out, stderr_text) = run_once(AgentRun {
            sandbox,
            worktree,
            argv,
            identity,
            prompt,
            bin,
            timeout,
            writes,
            early_ending,
            task_id,
            report,
            log,
        })
        .await?;

        let wall = Duration::from_millis(out.wall_ms as u64);
        if relaunch.again(&stderr_text, out.timed_out, wall) {
            report.emit(
                task_id,
                Event::Note {
                    text: &relaunch.note(),
                },
            );
            writeln!(log, "{}", relaunch.log_line(&stderr_text))?;
            continue;
        }
        return Ok((out, stderr_text));
    }
}

pub async fn run(l: Launch<'_>) -> Result<Outcome> {
    l.sandbox.map_or(Ok(()), |sb| sb.check_socket(l.worktree))?;
    match l.provider.runner {
        Runner::ClaudeCli => refusal::guarded_claude(l).await,
        Runner::CodexCli => {
            if l.no_tools {
                anyhow::bail!(
                    "the codex backend cannot yet guarantee no tool use for a directive step \
                     (docs/JOBS.md, \"Steps\"); route this step's role to a claude provider instead"
                );
            }
            let after = refusal::WriteBack::of(&l, &crate::login::CODEX);
            let out = refusal::read_stderr(run_codex(l).await);
            after.run().await;
            out
        }
        Runner::CopilotCli => {
            if l.no_tools {
                anyhow::bail!(
                    "the copilot backend cannot yet guarantee no tool use for a directive step \
                     (docs/JOBS.md, \"Steps\"); route this step's role to a claude provider instead"
                );
            }
            let after = refusal::WriteBack::of(&l, &crate::login::COPILOT);
            let out = refusal::read_stderr(run_copilot(l).await);
            after.run().await;
            out
        }
        Runner::Chat => {
            if !l.no_tools {
                anyhow::bail!(
                    "the chat backend has no tools at all, so it can only run a job's directive \
                     step (docs/JOBS.md, \"Steps\"); a task's code step needs an agent, route it \
                     to a claude or codex provider instead"
                );
            }
            run_chat(l).await
        }
        Runner::Jev => jev::run(l).await,
    }
}

/// One spawn of an agent CLI phase to exit or timeout, writing every raw
/// line to `log` and folding each JSON frame into `out`/`watch` through
/// `apply` — the plumbing the codex and copilot runners' phases share.
/// `apply` returns the early-ending text when a frame trips enough signals,
/// which ends the phase. Returns the exit code, and whether this phase
/// itself timed out; the caller decides what either means for the attempt
/// as a whole.
async fn run_json_phase(args: RunJsonPhase<'_>) -> Result<(Option<i32>, bool, String)> {
    let RunJsonPhase {
        l,
        argv,
        extra_env,
        start,
        log,
        out,
        watch,
        apply,
    } = args;
    let mut relaunch = Relaunch::default();
    loop {
        let began = Instant::now();
        let (code, timed_out, stderr) = run_json_phase_once(RunJsonPhase {
            l,
            argv,
            extra_env,
            start,
            log: &mut *log,
            out: &mut *out,
            watch: &mut *watch,
            apply: &mut *apply,
        })
        .await?;
        if relaunch.again(&stderr, timed_out, began.elapsed()) {
            l.report.emit(
                l.task_id,
                Event::Note {
                    text: &relaunch.note(),
                },
            );
            writeln!(log, "{}", relaunch.log_line(&stderr))?;
            continue;
        }
        return Ok((code, timed_out, stderr));
    }
}

/// One spawn of `run_json_phase`, without the bwrap relaunch.
async fn run_json_phase_once(args: RunJsonPhase<'_>) -> Result<(Option<i32>, bool, String)> {
    let RunJsonPhase {
        l,
        argv,
        extra_env,
        start,
        log,
        out,
        watch,
        apply,
    } = args;
    prepare_in(l.sandbox, l.worktree, extra_env, Phase::Agent).await;
    let mut child = spawn_retrying_etxtbsy(|| {
        let mut c = Command::from(command_in(
            l.sandbox,
            l.worktree,
            argv,
            extra_env,
            Phase::Agent,
        ));
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        c
    })
    .await
    .with_context(|| format!("spawning {}", argv[0]))?;

    let stderr = child.stderr.take().context("agent stderr")?;
    let stderr_task = tokio::spawn(async move {
        let mut s = String::new();
        BufReader::new(stderr).read_to_string(&mut s).await.ok();
        s
    });

    let deadline = tokio::time::Instant::now() + l.timeout;
    let stdout = child.stdout.take().context("agent stdout")?;
    let mut lines = BufReader::new(stdout).lines();
    let mut tripped_this_phase = false;

    let read = async {
        while let Some(line) = lines.next_line().await? {
            let Ok(mut v) = serde_json::from_str::<Value>(&line) else {
                writeln!(log, "{line}")?;
                continue;
            };
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "forge_ms".into(),
                    Value::from(start.elapsed().as_millis() as u64),
                );
            }
            writeln!(log, "{v}")?;
            if let Some(text) = apply(&v, out, watch) {
                writeln!(
                    log,
                    "{{\"type\":\"forge_early_end\",\"forge_ms\":{},\"signals\":{}}}",
                    start.elapsed().as_millis(),
                    serde_json::to_string(&text)?
                )?;
                l.report.emit(
                    l.task_id,
                    Event::Note {
                        text: &format!("early    stopped: {text}"),
                    },
                );
                tripped_this_phase = true;
                break;
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    let mut timed_out = false;
    let mut exit_code = None;
    match tokio::time::timeout_at(deadline, read).await {
        Ok(r) => {
            r?;
            if tripped_this_phase {
                child.kill().await.ok();
                child.wait().await.ok();
            } else {
                match tokio::time::timeout_at(deadline, child.wait()).await {
                    Ok(status) => exit_code = status?.code(),
                    Err(_) => timed_out = true,
                }
            }
        }
        Err(_) => timed_out = true,
    }
    if timed_out {
        child.kill().await.ok();
        child.wait().await.ok();
        writeln!(
            log,
            "{{\"type\":\"forge_timeout\",\"after_secs\":{}}}",
            l.timeout.as_secs()
        )?;
    }

    let stderr_text = stderr_task.await.unwrap_or_default();
    Ok((exit_code, timed_out, stderr_text))
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_environment_excludes_provider_variables_but_accepts_explicit_values() {
        for key in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "CODEX_HOME",
            "CODEX_API_KEY",
            "CODEX_OTHER",
            "COPILOT_HOME",
            "COPILOT_GITHUB_TOKEN",
            "COPILOT_OTHER",
            "CLAUDE_CONFIG_DIR",
        ] {
            assert!(inherited_env_allowed(key, Phase::Agent), "{key}");
            assert!(!inherited_env_allowed(key, Phase::Check), "{key}");
        }
        for phase in [Phase::Agent, Phase::Check] {
            assert!(inherited_env_allowed("PATH", phase));
            assert!(inherited_env_allowed("LC_ALL", phase));
            assert!(!inherited_env_allowed("UNRELATED_SECRET", phase));
        }
        let dir = tempfile::tempdir().unwrap();
        let argv = vec!["/usr/bin/env".into()];
        let extra = vec![("ANTHROPIC_API_KEY".into(), "explicit-check-key".into())];
        let output = command_in(None, dir.path(), &argv, &extra, Phase::Check)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == "ANTHROPIC_API_KEY=explicit-check-key")
        );
    }

    #[test]
    fn strict_schema_requires_every_key_of_every_object_and_keeps_the_rest() {
        let strict = inputs::strict_schema(crate::envelope::SCHEMA).unwrap();
        let v: serde_json::Value = serde_json::from_str(&strict).unwrap();
        fn check(v: &serde_json::Value) {
            if let Some(props) = v.get("properties").and_then(|p| p.as_object()) {
                let required: Vec<&str> = v["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r.as_str().unwrap())
                    .collect();
                for k in props.keys() {
                    assert!(required.contains(&k.as_str()), "{k} not required");
                }
                assert_eq!(v["additionalProperties"], false);
            }
            match v {
                serde_json::Value::Object(m) => m.values().for_each(check),
                serde_json::Value::Array(a) => a.iter().for_each(check),
                _ => {}
            }
        }
        check(&v);
        // The optional needs_input object now requires path, kind, options, context, to.
        let ni = &v["properties"]["needs_input"]["anyOf"][1];
        let req: Vec<&str> = ni["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();
        for k in [
            "question",
            "tried",
            "path",
            "kind",
            "options",
            "context",
            "checkpoint",
            "to",
        ] {
            assert!(req.contains(&k), "{k}");
        }
        // Nothing became nullable and the enum survived.
        assert_eq!(ni["properties"]["path"]["type"], "string");
        assert_eq!(
            ni["properties"]["kind"]["enum"].as_array().unwrap().len(),
            4
        );
        // An envelope written the strict way (every key present, optional ones empty) validates
        // against it and parses into the same Envelope as before. `changes` is not among its
        // keys: the model is never asked for it, since git says what changed (Token cost, C2).
        let doc = serde_json::json!({
            "schema_version": 1, "summary": "did it",
            "needs_input": {"question": "which?", "tried": "x", "path": "", "kind": "question",
                             "options": [], "context": "", "checkpoint": null, "to": ""},
            "checks_run": [{"check": "test", "passed": true, "notes": ""}],
            "claims": [{"claim": "c", "evidence": ""}]
        });
        jsonschema::validate(&v, &doc).unwrap();
        let e: crate::envelope::Envelope = serde_json::from_value(doc).unwrap();
        assert_eq!(e.needs_input.as_ref().unwrap().to.as_deref(), Some(""));
        assert!(e.changes.is_empty());
        // And every other schema the runners send is accepted by the transform.
        for s in [
            crate::supervisor::SCHEMA,
            crate::assess::SCHEMA,
            crate::deploy_look::SCHEMA,
        ] {
            inputs::strict_schema(s).unwrap();
        }
    }
    use super::*;
    use crate::config::EarlyEnding;
    use serde_json::json;
    use std::path::PathBuf;

    pub(super) fn thresholds(
        no_edit_calls: u32,
        edits_without_commit: u32,
        repeats: u32,
        signals_to_end: u32,
    ) -> EarlyEnding {
        EarlyEnding {
            no_edit_calls,
            edits_without_commit,
            repeats,
            signals_to_end,
        }
    }

    #[test]
    fn non_default_thresholds_trip_at_the_configured_count() {
        let mut w = Watch::new(thresholds(3, 100, 100, 2));
        for _ in 0..3 {
            w.saw("Read", &Value::Null);
        }
        let tripped = w.tripped(true);
        assert_eq!(tripped.len(), 1);
        assert_eq!(tripped[0].0, "no-edit");
        assert!(
            w.should_end(true).is_none(),
            "only one of the two required signals has tripped"
        );
    }

    #[test]
    fn should_end_fires_once_enough_signals_trip_at_custom_thresholds() {
        let mut w = Watch::new(thresholds(100, 2, 3, 2));
        w.saw("Edit", &Value::Null);
        w.saw("Edit", &Value::Null);
        for _ in 0..3 {
            w.saw("Bash", &json!({"command": "grep foo"}));
        }
        let ended = w.should_end(true).expect("two signals tripped together");
        let kinds: Vec<_> = ended.iter().map(|(k, _)| *k).collect();
        assert!(kinds.contains(&"uncommitted"));
        assert!(kinds.contains(&"repeat"));
    }

    #[test]
    fn signals_to_end_zero_disables_early_ending() {
        let mut w = Watch::new(thresholds(1, 1, 1, 0));
        w.saw("Read", &Value::Null);
        assert!(
            !w.tripped(true).is_empty(),
            "the signal itself still trips at its threshold"
        );
        assert!(
            w.should_end(true).is_none(),
            "signals_to_end = 0 means nothing ever ends the run"
        );
    }

    #[test]
    fn near_reports_signals_close_to_but_under_their_threshold() {
        let mut w = Watch::new(thresholds(10, 100, 100, 2));
        for _ in 0..9 {
            w.saw("Read", &Value::Null);
        }
        assert!(w.tripped(true).is_empty());
        assert_eq!(w.near(true), vec!["no-edit"]);
    }

    async fn run_counting_script(dir: &std::path::Path, body: &str) -> (Outcome, String) {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("agent.sh");
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut log = tempfile::NamedTempFile::new().unwrap();
        let report = crate::report::Reporter::new(false, None);
        run_with_relaunch(AgentRun {
            sandbox: None,
            worktree: dir,
            argv: &[script.to_string_lossy().to_string()],
            identity: &[],
            prompt: "prompt",
            bin: "agent.sh",
            timeout: Duration::from_secs(5),
            writes: true,
            early_ending: thresholds(0, 0, 0, 0),
            task_id: 1,
            report: &report,
            log: log.as_file_mut(),
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn relaunches_up_to_three_times_on_the_bwrap_bind_mount_race() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("count");
        let body = format!(
            "#!/bin/sh\n\
             n=$(cat {c} 2>/dev/null || echo 0); n=$((n + 1)); echo $n > {c}\n\
             if [ \"$n\" -lt 3 ]; then\n\
             echo \"bwrap: Can't bind mount /h/.claude.json on /h/.claude.json: \
             Unable to mount source on destination: No such file or directory\" >&2\n\
             exit 1\n\
             fi\n\
             exit 0\n",
            c = counter.display()
        );
        let (out, stderr_text) = run_counting_script(dir.path(), &body).await;
        assert!(!out.timed_out);
        assert!(
            stderr_text.trim().is_empty(),
            "the surviving run's own stderr is clean: {stderr_text}"
        );
        let launches: u32 = std::fs::read_to_string(&counter)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(launches, 3, "two relaunches, then the run that succeeds");
    }

    #[tokio::test]
    async fn does_not_relaunch_on_unrelated_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("count");
        let body = format!(
            "#!/bin/sh\n\
             n=$(cat {c} 2>/dev/null || echo 0); n=$((n + 1)); echo $n > {c}\n\
             echo 'agent: something else went wrong' >&2\n\
             exit 1\n",
            c = counter.display()
        );
        let (_out, stderr_text) = run_counting_script(dir.path(), &body).await;
        assert!(stderr_text.contains("something else went wrong"));
        let launches: u32 = std::fs::read_to_string(&counter)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(launches, 1, "an unrelated failure is never relaunched");
    }

    // Reason: test fixture helper keeps independently varied inputs explicit.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn test_launch<'a>(
        worktree: &'a Path,
        report: &'a Reporter,
        provider: &'a Provider,
        log_path: &'a PathBuf,
        schema: &'a str,
        no_tools: bool,
        resume: Option<&'a str>,
    ) -> Launch<'a> {
        Launch {
            task_id: 1,
            worktree,
            identity: Vec::new(),
            prompt: "do the task",
            system: "",
            model: "sonnet",
            max_turns: 3,
            timeout: Duration::from_secs(5),
            check_timeout: Duration::ZERO,
            log_path,
            sandbox: None,
            report,
            step: "step",
            provider,
            resume,
            start_sha: "",
            writes: false,
            schema,
            early_ending: thresholds(100, 100, 100, 2),
            no_tools,
            judgment: None,
        }
    }

    #[test]
    fn runner_from_str_accepts_copilot() {
        assert_eq!("copilot-cli".parse::<Runner>().unwrap(), Runner::CopilotCli);
        assert_eq!(Runner::CopilotCli.as_str(), "copilot-cli");
        assert!(
            "cowork-cli"
                .parse::<Runner>()
                .unwrap_err()
                .contains("copilot-cli")
        );
    }

    #[test]
    fn runner_from_str_accepts_chat() {
        assert_eq!("chat".parse::<Runner>().unwrap(), Runner::Chat);
        assert_eq!(Runner::Chat.as_str(), "chat");
        let err = "bogus".parse::<Runner>().unwrap_err();
        assert!(err.contains("chat"), "{err}");
    }
}
