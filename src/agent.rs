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
    pub wall_ms: u128,
    pub result_text: String,
    /// The structured result the CLI produced against the envelope schema,
    /// as raw JSON; `None` when the result frame carried none.
    pub structured: Option<String>,
    /// Last rate-limit sample seen on the stream, per window.
    pub rate_limits: RateLimits,
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
                "PATH" | "HOME" | "LANG" | "TERM" | "CLAUDE_CONFIG_DIR"
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
}

pub async fn run(l: Launch<'_>) -> Result<Outcome> {
    let bin = agent_bin_for(l.step);
    let argv: Vec<String> = [
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
        crate::envelope::SCHEMA,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
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
    let stdout = child.stdout.take().context("agent stdout")?;
    let mut lines = BufReader::new(stdout).lines();

    let read = async {
        while let Some(line) = lines.next_line().await? {
            writeln!(log, "{line}")?;
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
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
                                    l.report.emit(
                                        l.task_id,
                                        Event::ToolCall {
                                            name: b["name"].as_str().unwrap_or("?"),
                                        },
                                    );
                                }
                            }
                        }
                    }
                }
                Some("result") => {
                    out.got_result = true;
                    out.is_error = v["is_error"].as_bool().unwrap_or(false);
                    out.num_turns = v["num_turns"].as_i64().unwrap_or(0);
                    out.cost_usd = v["total_cost_usd"].as_f64();
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
                }
                _ => {}
            }
        }
        Ok::<(), anyhow::Error>(())
    };

    match tokio::time::timeout_at(deadline, read).await {
        Ok(r) => {
            r?;
            match tokio::time::timeout_at(deadline, child.wait()).await {
                Ok(status) => out.exit_code = status?.code(),
                Err(_) => out.timed_out = true,
            }
        }
        Err(_) => out.timed_out = true,
    }
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
