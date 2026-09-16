//! Spawn the agent CLI in the worktree and read its stream-json output
//! under a wall-clock timeout. The raw stream is the attempt's log, prompt
//! first. Numbers Forge records come from the CLI's accounting or Forge's
//! own clock, never from the model's prose.

use crate::report::{Event, Reporter};
use crate::sandbox::Sandbox;
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
}

/// Subscription usage as the CLI reports it: utilization is 0..1 of the
/// window, resets_at is unix seconds. On subscription billing this, not
/// the notional dollar figure, is the real budget.
#[derive(Default, Debug, Clone, Copy)]
pub struct RateLimits {
    pub five_hour: Option<(f64, i64)>,
    pub seven_day: Option<(f64, i64)>,
}

/// Which agent CLI backs a provider: the two ways Forge knows to build a
/// launch's argv and env and to parse its event stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Runner {
    #[default]
    ClaudeCli,
    CodexCli,
}

impl Runner {
    pub fn as_str(self) -> &'static str {
        match self {
            Runner::ClaudeCli => "claude-cli",
            Runner::CodexCli => "codex-cli",
        }
    }
}

impl std::str::FromStr for Runner {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "claude-cli" => Ok(Runner::ClaudeCli),
            "codex-cli" => Ok(Runner::CodexCli),
            other => Err(format!(
                "unknown runner {other:?}; expected \"claude-cli\" or \"codex-cli\""
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
    /// This provider's own rate-window caps (fractions of the window), as
    /// `[providers.<name>]` may override; default to the operator's
    /// `[budget]` caps when it does not (see `config::build_providers`).
    /// The worker holds this provider alone once its own latest sample
    /// reaches its own cap (see `worker::window_hold`).
    pub five_hour_max: f64,
    pub seven_day_max: f64,
}

impl Default for Provider {
    fn default() -> Self {
        Provider {
            name: "anthropic".into(),
            runner: Runner::ClaudeCli,
            model: Some("sonnet".into()),
            base_url: None,
            env: Vec::new(),
            extra_args: Vec::new(),
            notes: None,
            price_input_per_million: 0.0,
            price_output_per_million: 0.0,
            five_hour_max: 0.9,
            seven_day_max: 0.95,
        }
    }
}

pub fn agent_bin() -> String {
    std::env::var("FORGE2_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string())
}

/// The codex CLI, `FORGE2_CODEX_BIN` overridden (the codex-cli fake in
/// tests, an alternate build on an operator's machine).
pub fn codex_bin() -> String {
    std::env::var("FORGE2_CODEX_BIN").unwrap_or_else(|_| "codex".to_string())
}

/// `codex_bin`'s per-step override, `FORGE2_CODEX_BIN_<STEP>`; see
/// `agent_bin_for`, which does the same for the claude CLI.
pub fn codex_bin_for(step: &str) -> String {
    let key = format!(
        "FORGE2_CODEX_BIN_{}",
        step.to_ascii_uppercase().replace('-', "_")
    );
    std::env::var(key).unwrap_or_else(|_| codex_bin())
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

/// A step may run a different agent binary through FORGE2_CLAUDE_BIN_<STEP>
/// (upper-cased action name), which is how the test suite plays every
/// role in a workflow with a different script.
pub fn agent_bin_for(step: &str) -> String {
    let key = format!(
        "FORGE2_CLAUDE_BIN_{}",
        step.to_ascii_uppercase().replace('-', "_")
    );
    std::env::var(key).unwrap_or_else(|_| agent_bin())
}

/// The environment the agent and the checks see, sandboxed or not. This is
/// the one list; the sandbox overrides HOME on top of it.
pub fn agent_env() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(k, _)| {
            matches!(
                k.as_str(),
                "PATH"
                    | "HOME"
                    | "LANG"
                    | "TERM"
                    | "CLAUDE_CONFIG_DIR"
                    | "CODEX_HOME"
                    | "FAKE_SLEEP"
            ) || ["LC_", "ANTHROPIC_", "CODEX_"]
                .iter()
                .any(|p| k.starts_with(p))
        })
        .collect()
}

/// A command for `argv` in the worktree, through the sandbox when there is
/// one, with the agent environment plus `extra_env`.
pub fn command_in(
    sandbox: Option<&Sandbox>,
    worktree: &Path,
    argv: &[String],
    extra_env: &[(String, String)],
) -> std::process::Command {
    let mut env = agent_env();
    env.extend(extra_env.iter().cloned());
    match sandbox {
        Some(sb) => sb.command(worktree, argv, &env),
        None => {
            let mut c = std::process::Command::new(&argv[0]);
            c.args(&argv[1..])
                .current_dir(worktree)
                .env_clear()
                .envs(env);
            c
        }
    }
}

pub struct Launch<'a> {
    pub task_id: i64,
    pub worktree: &'a Path,
    pub prompt: &'a str,
    pub model: &'a str,
    pub max_turns: u32,
    pub timeout: Duration,
    pub log_path: &'a Path,
    pub sandbox: Option<&'a Sandbox>,
    pub report: &'a Reporter,
    pub step: &'a str,
    /// The provider this step runs under: which CLI, and what it adds to
    /// the launch's argv and env.
    pub provider: &'a Provider,
    /// A CLI session to continue instead of starting fresh.
    pub resume: Option<&'a str>,
    /// Whether this step is expected to change files (code, tests). A
    /// read-only step (review, plan) is never faulted for not editing.
    pub writes: bool,
    /// The JSON schema the CLI holds the structured result to; the
    /// envelope for every directive, the supervisor's own for it.
    pub schema: &'a str,
    /// Thresholds for `Watch`, the operator's `[early_ending]` config.
    pub early_ending: crate::config::EarlyEnding,
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

/// The launch race this covers: two sandboxes seed `$HOME/.claude.json`
/// from the same host file at once, and bwrap's own bind-mount setup (not
/// the claude CLI's rename) loses. bwrap always reports it exactly this
/// way, so matching the message is precise enough without a regex crate.
fn is_transient_bwrap_failure(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        let Some(rest) = line.trim_start().strip_prefix("bwrap: Can") else {
            return false;
        };
        let mut chars = rest.chars();
        chars.next().is_some() && chars.as_str().starts_with("t bind mount")
    })
}

/// One spawn of `argv` through to exit or timeout: the same shape `run` has
/// always had, just factored out so `run` can retry it on the bwrap
/// bind-mount race without re-deriving `bin`/`argv` from `Launch` (which
/// would need a live `claude` binary in tests) and without disturbing the
/// per-line `forge_ms` timestamps, which must measure from this attempt's
/// own spawn, not from whenever a caller-side retry loop happens to notice
/// it finished.
#[allow(clippy::too_many_arguments)]
async fn run_once(
    sandbox: Option<&Sandbox>,
    worktree: &Path,
    argv: &[String],
    identity: &[(String, String)],
    prompt: &str,
    bin: &str,
    timeout: Duration,
    writes: bool,
    early_ending: crate::config::EarlyEnding,
    task_id: i64,
    report: &Reporter,
    log: &mut File,
) -> Result<(Outcome, String)> {
    let mut child = Command::from(command_in(sandbox, worktree, argv, identity))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
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
                    out.got_result = true;
                    out.is_error = v["is_error"].as_bool().unwrap_or(false);
                    if let Some(id) = v["session_id"].as_str() {
                        out.session_id = Some(id.to_string());
                    }
                    out.max_turns_hit = v["subtype"].as_str() == Some("error_max_turns");
                    let text = v["result"].as_str().unwrap_or("").to_ascii_lowercase();
                    if out.is_error && (text.contains("rate limit") || text.contains("rate-limit"))
                    {
                        out.rate_limited = true;
                    }
                    out.num_turns = v["num_turns"].as_i64().unwrap_or(0);
                    out.cost_usd = v["total_cost_usd"].as_f64();
                    out.input_tokens = v["usage"]["input_tokens"].as_i64();
                    out.output_tokens = v["usage"]["output_tokens"].as_i64();
                    out.cache_read_input_tokens = v["usage"]["cache_read_input_tokens"].as_i64();
                    out.cache_creation_input_tokens =
                        v["usage"]["cache_creation_input_tokens"].as_i64();
                    out.result_text = v["result"].as_str().unwrap_or("").to_string();
                    out.structured = match &v["structured_output"] {
                        Value::Null => None,
                        other => Some(other.to_string()),
                    };
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
/// seed-bind race documented on `is_transient_bwrap_failure`: the child
/// exits within two seconds with that message on stderr. That is a launch
/// failure, not an attempt, so it gets a few silent relaunches rather than
/// burning one of the attempt's own retries. Any other quick exit (a real
/// crash, a fast fake in tests) is returned as is.
#[allow(clippy::too_many_arguments)]
async fn run_with_relaunch(
    sandbox: Option<&Sandbox>,
    worktree: &Path,
    argv: &[String],
    identity: &[(String, String)],
    prompt: &str,
    bin: &str,
    timeout: Duration,
    writes: bool,
    early_ending: crate::config::EarlyEnding,
    task_id: i64,
    report: &Reporter,
    log: &mut File,
) -> Result<(Outcome, String)> {
    const MAX_RELAUNCHES: u32 = 3;
    let mut relaunches = 0u32;
    loop {
        let (out, stderr_text) = run_once(
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
        )
        .await?;

        let quick_exit = !out.timed_out && out.wall_ms < 2_000;
        if quick_exit && relaunches < MAX_RELAUNCHES && is_transient_bwrap_failure(&stderr_text) {
            relaunches += 1;
            let msg = format!(
                "transient bwrap bind-mount failure on launch, relaunching (attempt {relaunches}/{MAX_RELAUNCHES})"
            );
            report.emit(task_id, Event::Note { text: &msg });
            writeln!(
                log,
                "{{\"type\":\"forge_relaunch\",\"attempt\":{relaunches},\"reason\":{}}}",
                serde_json::to_string(&stderr_text)?
            )?;
            continue;
        }
        return Ok((out, stderr_text));
    }
}

pub async fn run(l: Launch<'_>) -> Result<Outcome> {
    match l.provider.runner {
        Runner::ClaudeCli => run_claude(l).await,
        Runner::CodexCli => run_codex(l).await,
    }
}

async fn run_claude(l: Launch<'_>) -> Result<Outcome> {
    // The binary itself, never a version-manager shim: a shim inside the
    // sandbox reaches for state the sandbox does not have (a global tool
    // config, a registry cache, a writable shims directory) and dies
    // before the agent starts. Forge 1 learned this the same way.
    let bin = real_bin(&agent_bin_for(l.step));
    let mut argv: Vec<String> = [
        bin.as_str(),
        "--print",
        "--verbose",
        "--output-format",
        "stream-json",
        "--dangerously-skip-permissions",
        "--model",
        l.model,
        "--max-turns",
        &l.max_turns.to_string(),
        "--json-schema",
        l.schema,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    argv.extend(l.provider.extra_args.iter().cloned());
    if let Some(id) = l.resume {
        argv.push("--resume".into());
        argv.push(id.to_string());
    }
    let mut identity = crate::git::identity(&l.worktree.join(".git")).await;
    identity.extend(l.provider.env.iter().cloned());
    let mut log =
        File::create(l.log_path).with_context(|| format!("creating {}", l.log_path.display()))?;
    writeln!(
        log,
        "{{\"type\":\"forge_prompt\",\"text\":{}}}",
        serde_json::to_string(l.prompt)?
    )?;

    let (out, stderr_text) = run_with_relaunch(
        l.sandbox,
        l.worktree,
        &argv,
        &identity,
        l.prompt,
        &bin,
        l.timeout,
        l.writes,
        l.early_ending,
        l.task_id,
        l.report,
        &mut log,
    )
    .await?;

    if !stderr_text.trim().is_empty() {
        writeln!(
            log,
            "{{\"type\":\"forge_stderr\",\"text\":{}}}",
            serde_json::to_string(&stderr_text)?
        )?;
    }
    Ok(out)
}

/// Applies one parsed line of codex's `--json` event stream to `out` and
/// `watch`; the side effects that need the log file or the reporter (every
/// line gets written verbatim, a command execution is reported as a tool
/// call) are the caller's, in `run_codex`, so this stays pure enough to
/// unit-test against captured lines. Returns the early-ending signals'
/// text when `Watch` says enough of them tripped to stop the run.
fn apply_codex_event(
    v: &Value,
    out: &mut Outcome,
    watch: &mut Watch,
    writes: bool,
) -> Option<String> {
    match v["type"].as_str() {
        Some("thread.started") => {
            if let Some(id) = v["thread_id"].as_str() {
                out.session_id = Some(id.to_string());
            }
        }
        Some("item.started") => {
            if v["item"]["type"] == "command_execution" {
                out.tool_calls += 1;
                let cmd = v["item"]["command"].as_str().unwrap_or("").to_string();
                watch.saw("Bash", &serde_json::json!({ "command": cmd }));
                if let Some(tripped) = watch.should_end(writes) {
                    let text = tripped
                        .iter()
                        .map(|(_, w)| w.as_str())
                        .collect::<Vec<_>>()
                        .join("; ");
                    out.ended_early = Some(text.clone());
                    return Some(text);
                }
            }
        }
        Some("item.completed") => match v["item"]["type"].as_str() {
            // Codex reports warnings as error items too ("Model metadata for
            // `qwen3-coder:30b` not found. Defaulting to fallback metadata"),
            // before it goes on to work. An error item is a failure only if
            // no result follows; a result clears it (task 288 was failed for
            // a warning while its structured result was a valid question).
            Some("error") => out.is_error = true,
            Some("agent_message") => {
                let text = v["item"]["text"].as_str().unwrap_or("").to_string();
                out.got_result = true;
                out.is_error = false;
                out.structured = serde_json::from_str::<Value>(&text)
                    .ok()
                    .map(|_| text.clone());
                out.result_text = text;
            }
            _ => {}
        },
        Some("turn.completed") => {
            out.num_turns += 1;
            let u = &v["usage"];
            let input = u["input_tokens"].as_i64().unwrap_or(0);
            let cached = u["cached_input_tokens"].as_i64().unwrap_or(0);
            let output = u["output_tokens"].as_i64().unwrap_or(0)
                + u["reasoning_output_tokens"].as_i64().unwrap_or(0);
            out.input_tokens = Some(out.input_tokens.unwrap_or(0) + input);
            out.cache_read_input_tokens = Some(out.cache_read_input_tokens.unwrap_or(0) + cached);
            out.output_tokens = Some(out.output_tokens.unwrap_or(0) + output);
        }
        _ => {}
    }
    None
}

/// The codex-cli backend: `codex exec --skip-git-repo-check --json -C
/// <worktree> [-m <model>] --output-schema <file> <prompt>`, sandboxed with
/// `-s workspace-write` when Forge's own sandbox is off, or
/// `--dangerously-bypass-approvals-and-sandbox` when the attempt already
/// runs inside one (bubblewrap) and codex's own would only be redundant.
/// Resuming swaps in `resume <thread_id>` after `exec`. Stdin is always
/// closed: codex blocks forever reading it otherwise, unlike the claude CLI,
/// which takes the prompt on stdin.
async fn run_codex(l: Launch<'_>) -> Result<Outcome> {
    let bin = real_bin(&codex_bin_for(l.step));
    // The schema is text (`envelope::SCHEMA`), but codex takes a file, and
    // codex reads it inside the sandbox, where Forge's home is an empty
    // tmpfs. The worktree is the one directory bound read-write for the
    // attempt, and its `.git` is invisible to `git status`, so the file
    // lives there (tasks 274-286 exited at launch: "Failed to read output
    // schema file", written beside the log under FORGE2_HOME).
    let schema_path = l
        .worktree
        .join(".git")
        .join(format!("forge-{}-schema.json", l.step));
    std::fs::write(&schema_path, l.schema)
        .with_context(|| format!("writing {}", schema_path.display()))?;

    let mut argv: Vec<String> = vec![bin.clone(), "exec".to_string()];
    if let Some(id) = l.resume {
        argv.push("resume".into());
        argv.push(id.to_string());
    }
    argv.push("--skip-git-repo-check".into());
    argv.push("--json".into());
    argv.push("-C".into());
    argv.push(l.worktree.display().to_string());
    if l.sandbox.is_some() {
        argv.push("--dangerously-bypass-approvals-and-sandbox".into());
    } else {
        argv.push("-s".into());
        argv.push("workspace-write".into());
    }
    if !l.model.is_empty() {
        argv.push("-m".into());
        argv.push(l.model.to_string());
    }
    argv.push("--output-schema".into());
    argv.push(schema_path.display().to_string());
    argv.extend(l.provider.extra_args.iter().cloned());
    argv.push(l.prompt.to_string());

    let mut extra_env = crate::git::identity(&l.worktree.join(".git")).await;
    extra_env.extend(l.provider.env.iter().cloned());

    let mut log =
        File::create(l.log_path).with_context(|| format!("creating {}", l.log_path.display()))?;
    writeln!(
        log,
        "{{\"type\":\"forge_prompt\",\"text\":{}}}",
        serde_json::to_string(l.prompt)?
    )?;

    let mut child = Command::from(command_in(l.sandbox, l.worktree, &argv, &extra_env))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawning {bin}"))?;

    let stderr = child.stderr.take().context("agent stderr")?;
    let stderr_task = tokio::spawn(async move {
        let mut s = String::new();
        BufReader::new(stderr).read_to_string(&mut s).await.ok();
        s
    });

    let start = Instant::now();
    let deadline = tokio::time::Instant::now() + l.timeout;
    let mut out = Outcome {
        session_id: l.resume.map(|s| s.to_string()),
        ..Outcome::default()
    };
    let mut watch = Watch::new(l.early_ending);
    let stdout = child.stdout.take().context("agent stdout")?;
    let mut lines = BufReader::new(stdout).lines();

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
            if v["type"] == "item.started" && v["item"]["type"] == "command_execution" {
                let name = v["item"]["command"].as_str().unwrap_or("command_execution");
                l.report.emit(l.task_id, Event::ToolCall { name });
            }
            if let Some(text) = apply_codex_event(&v, &mut out, &mut watch, l.writes) {
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
                break;
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
    out.early_signals = watch.tripped(l.writes).iter().map(|(k, _)| *k).collect();
    out.early_near = watch.near(l.writes);
    if out.timed_out {
        child.kill().await.ok();
        child.wait().await.ok();
        writeln!(
            log,
            "{{\"type\":\"forge_timeout\",\"after_secs\":{}}}",
            l.timeout.as_secs()
        )?;
    }
    if !out.is_error {
        out.is_error = out.exit_code.is_some_and(|c| c != 0);
    }
    out.wall_ms = start.elapsed().as_millis();

    // Codex reports no cost of its own; the operator's per-provider price
    // table (0 for a local model) turns its token counts into one.
    if let (Some(input), Some(output)) = (out.input_tokens, out.output_tokens) {
        out.cost_usd = Some(
            input as f64 * l.provider.price_input_per_million / 1_000_000.0
                + output as f64 * l.provider.price_output_per_million / 1_000_000.0,
        );
    }

    let stderr_text = stderr_task.await.unwrap_or_default();
    if !stderr_text.trim().is_empty() {
        writeln!(
            log,
            "{{\"type\":\"forge_stderr\",\"text\":{}}}",
            serde_json::to_string(&stderr_text)?
        )?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EarlyEnding;
    use serde_json::json;

    fn thresholds(
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

    #[test]
    fn is_transient_bwrap_failure_matches_the_bind_mount_race() {
        let msg = "bwrap: Can't bind mount /home/ronin/.claude.json on \
                    /home/ronin/.claude.json: Unable to mount source on \
                    destination: No such file or directory";
        assert!(is_transient_bwrap_failure(msg));
        // Leading indentation on the line is still a match.
        assert!(is_transient_bwrap_failure(&format!("  {msg}")));
    }

    #[test]
    fn is_transient_bwrap_failure_ignores_other_stderr() {
        assert!(!is_transient_bwrap_failure(""));
        assert!(!is_transient_bwrap_failure("agent crashed: out of memory"));
        assert!(!is_transient_bwrap_failure(
            "bwrap: Can't create file /run/forge/seed/claude.json: Permission denied"
        ));
        assert!(!is_transient_bwrap_failure(
            "bwrap: execvp claude: No such file or directory"
        ));
    }

    async fn run_counting_script(dir: &std::path::Path, body: &str) -> (Outcome, String) {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("agent.sh");
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut log = tempfile::NamedTempFile::new().unwrap();
        let report = crate::report::Reporter::new(false, None);
        run_with_relaunch(
            None,
            dir,
            &[script.to_string_lossy().to_string()],
            &[],
            "prompt",
            "agent.sh",
            Duration::from_secs(5),
            true,
            thresholds(0, 0, 0, 0),
            1,
            &report,
            log.as_file_mut(),
        )
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

    /// Captured lines from a codex `exec --json` run: a thread starting, one
    /// command execution, an error item, a final assistant message carrying
    /// the envelope as its text, and the turn's usage.
    fn codex_fixture() -> Vec<&'static str> {
        vec![
            r#"{"type":"thread.started","thread_id":"codex-sess-1"}"#,
            r#"{"type":"item.started","item":{"id":"i0","type":"command_execution","command":"echo 42 > answer.txt"}}"#,
            r#"{"type":"item.completed","item":{"id":"i0","type":"command_execution","command":"echo 42 > answer.txt","exit_code":0}}"#,
            r#"{"type":"item.completed","item":{"id":"i1","type":"error","message":"a tool call failed"}}"#,
            r#"{"type":"item.completed","item":{"id":"i2","type":"agent_message","text":"{\"schema_version\":1,\"summary\":\"wrote 42\",\"needs_input\":null,\"changes\":[{\"path\":\"answer.txt\",\"kind\":\"added\"}],\"checks_run\":[],\"claims\":[]}"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":10,"output_tokens":50,"reasoning_output_tokens":5}}"#,
        ]
    }

    fn run_codex_fixture(lines: &[&str], thresholds: EarlyEnding) -> Outcome {
        let mut out = Outcome::default();
        let mut watch = Watch::new(thresholds);
        for line in lines {
            let v: Value = serde_json::from_str(line).unwrap();
            if let Some(text) = apply_codex_event(&v, &mut out, &mut watch, true) {
                out.ended_early = Some(text);
                break;
            }
        }
        out
    }

    #[test]
    fn a_codex_error_item_with_no_result_is_a_failure() {
        let lines = [
            r#"{"type":"thread.started","thread_id":"codex-sess-2"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.completed","item":{"id":"i1","type":"error","message":"stream disconnected"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":5,"cached_input_tokens":0,"output_tokens":1,"reasoning_output_tokens":0}}"#,
        ];
        let out = run_codex_fixture(&lines, thresholds(100, 100, 100, 2));
        assert!(out.is_error, "no result followed the error item");
        assert!(!out.got_result);
    }

    #[test]
    fn codex_events_parse_into_the_outcome() {
        let out = run_codex_fixture(&codex_fixture(), thresholds(100, 100, 100, 2));
        assert_eq!(out.session_id.as_deref(), Some("codex-sess-1"));
        assert_eq!(out.tool_calls, 1);
        assert!(
            !out.is_error,
            "an error item before a valid result is a warning, not a failure"
        );
        assert!(out.got_result);
        assert_eq!(out.result_text, out.structured.clone().unwrap());
        let structured: Value = serde_json::from_str(&out.structured.unwrap()).unwrap();
        assert_eq!(structured["summary"], "wrote 42");
        assert_eq!(out.num_turns, 1);
        assert_eq!(out.input_tokens, Some(100));
        assert_eq!(out.cache_read_input_tokens, Some(10));
        // Reasoning tokens sum into the output count alongside the plain ones.
        assert_eq!(out.output_tokens, Some(55));
    }

    #[test]
    fn codex_command_executions_feed_the_early_ending_watch() {
        let lines = vec![
            r#"{"type":"thread.started","thread_id":"s"}"#,
            r#"{"type":"item.started","item":{"id":"i0","type":"command_execution","command":"grep foo"}}"#,
            r#"{"type":"item.completed","item":{"id":"i0","type":"command_execution","command":"grep foo","exit_code":0}}"#,
            r#"{"type":"item.started","item":{"id":"i1","type":"command_execution","command":"grep foo"}}"#,
            r#"{"type":"item.completed","item":{"id":"i1","type":"command_execution","command":"grep foo","exit_code":0}}"#,
        ];
        let out = run_codex_fixture(&lines, thresholds(100, 100, 2, 1));
        assert_eq!(
            out.ended_early.as_deref(),
            Some("`grep foo` run 2 times"),
            "the repeated command trips the same Watch the claude runner uses"
        );
    }
}
