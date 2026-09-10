//! Spawn the agent CLI in the worktree and read its stream-json output.
//! The raw stream is the attempt's log. Numbers Forge records come from the
//! CLI's accounting or Forge's own clock, never from the model's prose.

use crate::sandbox::Sandbox;
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

/// The environment an unsandboxed agent sees. Nothing else from the parent leaks.
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

pub struct Launch<'a> {
    pub worktree: &'a Path,
    pub repo_git_dir: &'a Path,
    pub prompt: &'a str,
    pub model: &'a str,
    pub max_turns: u32,
    pub log_path: &'a Path,
    pub sandbox: Option<&'a Sandbox>,
}

/// Git identity for commits made inside the sandbox, where the host's
/// ~/.gitconfig is invisible. Read from the repository (which includes the
/// global config) and passed as GIT_CONFIG_* environment.
fn git_identity(repo_git_dir: &Path) -> Vec<(String, String)> {
    let get = |key: &str| {
        Command::new("git")
            .arg("--git-dir")
            .arg(repo_git_dir)
            .args(["config", "--get", key])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let name = get("user.name").unwrap_or_else(|| "Forge".to_string());
    let email = get("user.email").unwrap_or_else(|| "forge@localhost".to_string());
    vec![
        ("GIT_CONFIG_COUNT".into(), "2".into()),
        ("GIT_CONFIG_KEY_0".into(), "user.name".into()),
        ("GIT_CONFIG_VALUE_0".into(), name),
        ("GIT_CONFIG_KEY_1".into(), "user.email".into()),
        ("GIT_CONFIG_VALUE_1".into(), email),
    ]
}

pub fn run(l: Launch) -> Result<Outcome> {
    let bin = std::env::var("FORGE2_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
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
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let start = Instant::now();
    let mut cmd = match l.sandbox {
        Some(sb) => sb.command(l.worktree, l.repo_git_dir, &argv),
        None => {
            let mut c = Command::new(&argv[0]);
            c.args(&argv[1..])
                .current_dir(l.worktree)
                .env_clear()
                .envs(passthrough_env());
            c
        }
    };
    cmd.envs(git_identity(l.repo_git_dir));
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {bin}"))?;

    {
        let mut stdin = child.stdin.take().context("agent stdin")?;
        stdin.write_all(l.prompt.as_bytes())?;
    }
    let stderr = child.stderr.take().context("agent stderr")?;
    let stderr_thread = std::thread::spawn(move || {
        let mut s = String::new();
        BufReader::new(stderr).read_to_string(&mut s).ok();
        s
    });

    let mut log =
        File::create(l.log_path).with_context(|| format!("creating {}", l.log_path.display()))?;
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
