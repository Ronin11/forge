//! Run one command as a check: in the worktree, through the sandbox, under
//! a timeout, with a bounded tail of its output. A claim from the agent is
//! not a result; this is.

use crate::sandbox::Sandbox;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CheckResult {
    pub level: String,
    pub name: String,
    pub ok: bool,
    pub exit: Option<i32>,
    pub ms: u128,
    pub timed_out: bool,
    pub tail: String,
}

pub async fn run_one(
    level: &str,
    name: &str,
    argv: &[String],
    cwd: &Path,
    repo_git_dir: &Path,
    sandbox: Option<&Sandbox>,
    timeout: Duration,
) -> CheckResult {
    let start = Instant::now();
    let mut r = CheckResult {
        level: level.to_string(),
        name: name.to_string(),
        ok: false,
        exit: None,
        ms: 0,
        timed_out: false,
        tail: String::new(),
    };
    let child = Command::from(crate::agent::command_in(
        sandbox,
        cwd,
        repo_git_dir,
        argv,
        &[],
    ))
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn();
    let child = match child {
        Ok(c) => c,
        Err(e) => {
            r.tail = format!("could not start: {e}");
            r.ms = start.elapsed().as_millis();
            return r;
        }
    };
    // On timeout the future is dropped, and with it the child, which kills it.
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            let tail: Vec<&str> = text.lines().rev().take(20).collect();
            r.tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
            r.ok = o.status.success();
            r.exit = o.status.code();
        }
        Ok(Err(e)) => r.tail = e.to_string(),
        Err(_) => {
            r.timed_out = true;
            r.tail = format!("timed out after {}s", timeout.as_secs());
        }
    }
    r.ms = start.elapsed().as_millis();
    r
}
