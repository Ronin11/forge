//! Spawn the agent CLI in the worktree and read its stream-json output.
//! The raw stream is the attempt's log. Numbers Forge records come from the
//! CLI's accounting or Forge's own clock, never from the model's prose.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(Default, Debug)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    pub got_result: bool,
    pub is_error: bool,
    pub num_turns: i64,
    pub tool_calls: i64,
    pub cost_usd: Option<f64>,
    pub wall_ms: u128,
    pub result_text: String,
}

/// The environment the agent sees. Nothing else from the parent leaks.
fn passthrough_env() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(k, _)| {
            matches!(
                k.as_str(),
                "PATH" | "HOME" | "USER" | "LANG" | "TERM" | "SSH_AUTH_SOCK" | "CLAUDE_CONFIG_DIR"
            ) || ["LC_", "XDG_", "ANTHROPIC_"]
                .iter()
                .any(|p| k.starts_with(p))
        })
        .collect()
}

pub fn run(
    cwd: &Path,
    prompt: &str,
    model: &str,
    max_turns: u32,
    log_path: &Path,
) -> Result<Outcome> {
    let bin = std::env::var("FORGE2_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
    let start = Instant::now();
    let mut child = Command::new(&bin)
        .args([
            "--print",
            "--verbose",
            "--output-format",
            "stream-json",
            "--dangerously-skip-permissions",
            "--model",
            model,
            "--max-turns",
            &max_turns.to_string(),
        ])
        .current_dir(cwd)
        .env_clear()
        .envs(passthrough_env())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {bin}"))?;

    {
        let mut stdin = child.stdin.take().context("agent stdin")?;
        stdin.write_all(prompt.as_bytes())?;
    }
    let stderr = child.stderr.take().context("agent stderr")?;
    let stderr_thread = std::thread::spawn(move || {
        let mut s = String::new();
        BufReader::new(stderr).read_to_string(&mut s).ok();
        s
    });

    let mut log =
        File::create(log_path).with_context(|| format!("creating {}", log_path.display()))?;
    let mut out = Outcome::default();
    let mut seen_tools: HashSet<String> = HashSet::new();
    let stdout = child.stdout.take().context("agent stdout")?;
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        writeln!(log, "{line}")?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v["type"].as_str() {
            Some("assistant") => {
                // The CLI repeats a message once per content block; count each
                // tool_use id once.
                if let Some(blocks) = v["message"]["content"].as_array() {
                    for b in blocks {
                        if b["type"] == "tool_use" {
                            let id = b["id"].as_str().unwrap_or("").to_string();
                            if seen_tools.insert(id) {
                                out.tool_calls += 1;
                                eprintln!("  ▸ {}", b["name"].as_str().unwrap_or("?"));
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
            }
            _ => {}
        }
    }
    let status = child.wait().context("waiting for agent")?;
    out.exit_code = status.code();
    out.wall_ms = start.elapsed().as_millis();

    let stderr_text = stderr_thread.join().unwrap_or_default();
    if !stderr_text.trim().is_empty() {
        writeln!(
            log,
            "{{\"type\":\"forge_stderr\",\"text\":{}}}",
            serde_json::to_string(&stderr_text)?
        )?;
    }
    Ok(out)
}
