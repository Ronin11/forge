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

pub fn agent_bin() -> String {
    std::env::var("FORGE2_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string())
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
                "PATH" | "HOME" | "LANG" | "TERM" | "CLAUDE_CONFIG_DIR" | "FAKE_SLEEP"
            ) || ["LC_", "ANTHROPIC_"].iter().any(|p| k.starts_with(p))
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

pub async fn run(l: Launch<'_>) -> Result<Outcome> {
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
    if let Some(id) = l.resume {
        argv.push("--resume".into());
        argv.push(id.to_string());
    }
    let identity = crate::git::identity(&l.worktree.join(".git")).await;
    let start = Instant::now();
    let deadline = tokio::time::Instant::now() + l.timeout;
    let mut child = Command::from(command_in(l.sandbox, l.worktree, &argv, &identity))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawning {bin}"))?;

    {
        let mut stdin = child.stdin.take().context("agent stdin")?;
        stdin.write_all(l.prompt.as_bytes()).await?;
        stdin.shutdown().await?;
    }
    let stderr = child.stderr.take().context("agent stderr")?;
    let stderr_task = tokio::spawn(async move {
        let mut s = String::new();
        BufReader::new(stderr).read_to_string(&mut s).await.ok();
        s
    });

    let mut log =
        File::create(l.log_path).with_context(|| format!("creating {}", l.log_path.display()))?;
    writeln!(
        log,
        "{{\"type\":\"forge_prompt\",\"text\":{}}}",
        serde_json::to_string(l.prompt)?
    )?;
    let mut out = Outcome::default();
    let mut seen_tools: HashSet<String> = HashSet::new();
    let mut watch = Watch::new(l.early_ending);
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
                                    l.report.emit(l.task_id, Event::ToolCall { name });
                                    watch.saw(name, &b["input"]);
                                }
                            }
                        }
                    }
                    if let Some(tripped) = watch.should_end(l.writes) {
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
                        l.report.emit(
                            l.task_id,
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
    out.wall_ms = start.elapsed().as_millis();

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
}
