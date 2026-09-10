//! Run one command as a check: in the worktree, through the sandbox, under
//! a timeout, with a bounded tail of its output and the names of failing
//! tests when the output is in a format Forge recognises. A claim from the
//! agent is not a result; this is.
//!
//! A check that backgrounds a server (`npm run preview &`) hands that
//! server its stdout, and waiting for output would then wait for the server
//! rather than for the check. So the check's exit decides, its process
//! group is killed after it exits, and the output is drained with a short
//! grace. Learned the hard way in Forge 1.

use crate::sandbox::Sandbox;
use serde::{Deserialize, Serialize};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// How much of a check's output Forge keeps: the tail, which is where a
/// test runner puts its failures.
const TAIL_BYTES: usize = 16 * 1024;
const DRAIN_GRACE: Duration = Duration::from_secs(2);

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct CheckResult {
    pub level: String,
    pub name: String,
    pub ok: bool,
    pub exit: Option<i32>,
    pub ms: u128,
    pub timed_out: bool,
    pub tail: String,
    #[serde(default)]
    pub failing_tests: Vec<String>,
}

/// Keeps the last `TAIL_BYTES` written to it.
#[derive(Default)]
struct Tail(Vec<u8>);

impl Tail {
    fn write(&mut self, chunk: &[u8]) {
        self.0.extend_from_slice(chunk);
        if self.0.len() > TAIL_BYTES {
            let cut = self.0.len() - TAIL_BYTES;
            self.0.drain(..cut);
        }
    }
    fn string(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }
}

async fn drain(mut r: impl AsyncReadExt + Unpin, tail: Arc<Mutex<Tail>>) {
    let mut buf = [0u8; 8192];
    while let Ok(n) = r.read(&mut buf).await {
        if n == 0 {
            break;
        }
        tail.lock()
            .unwrap_or_else(|p| p.into_inner())
            .write(&buf[..n]);
    }
}

/// Test names from the formats Forge recognises; unknown output yields
/// nothing rather than a guess.
pub fn failing_tests(out: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in out.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("--- FAIL: ") {
            // go test
            if let Some(n) = rest.split_whitespace().next() {
                names.push(n.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("FAILED ").filter(|r| r.contains("::")) {
            // pytest
            if let Some(n) = rest.split_whitespace().next() {
                names.push(n.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("✕ ").or_else(|| line.strip_prefix("✗ ")) {
            // jest
            names.push(rest.trim().to_string());
        }
    }
    names
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
        ..Default::default()
    };
    let mut std_cmd = crate::agent::command_in(sandbox, cwd, repo_git_dir, argv, &[]);
    // Unsandboxed checks get their own process group so a backgrounded
    // child can be killed with them; bwrap's --new-session does the same.
    std_cmd.process_group(0);
    let child = Command::from(std_cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            r.tail = format!("could not start: {e}");
            r.ms = start.elapsed().as_millis();
            return r;
        }
    };
    let pid = child.id();
    let tail = Arc::new(Mutex::new(Tail::default()));
    let mut readers = tokio::task::JoinSet::new();
    if let Some(out) = child.stdout.take() {
        readers.spawn(drain(out, tail.clone()));
    }
    if let Some(err) = child.stderr.take() {
        readers.spawn(drain(err, tail.clone()));
    }

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => {
            r.ok = status.success();
            r.exit = status.code();
        }
        Ok(Err(e)) => r.tail = e.to_string(),
        Err(_) => {
            r.timed_out = true;
            child.kill().await.ok();
            child.wait().await.ok();
        }
    }
    // The check has exited; nothing it left behind may outlive it.
    if let Some(pid) = pid {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .output()
            .await;
    }
    if tokio::time::timeout(DRAIN_GRACE, async {
        while readers.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        readers.abort_all();
    }
    let text = tail.lock().unwrap_or_else(|p| p.into_inner()).string();
    r.failing_tests = failing_tests(&text);
    if r.timed_out {
        r.tail = format!("{text}\n[forge] timed out after {}s", timeout.as_secs());
    } else if r.tail.is_empty() {
        r.tail = text;
    }
    r.ms = start.elapsed().as_millis();
    r
}

/// The last `n` lines, for display and feedback.
pub fn last_lines(s: &str, n: usize) -> String {
    let v: Vec<&str> = s.lines().rev().take(n).collect();
    v.into_iter().rev().collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_go_pytest_and_jest_failures_only() {
        let out = "\
=== RUN   TestA
--- FAIL: TestA (0.00s)
--- PASS: TestB (0.00s)
FAILED tests/test_x.py::test_one - AssertionError
FAILED not a pytest line
  ✕ renders the header (12 ms)
  ✓ renders the footer
random FAIL text
";
        assert_eq!(
            failing_tests(out),
            vec![
                "TestA",
                "tests/test_x.py::test_one",
                "renders the header (12 ms)"
            ]
        );
        assert!(failing_tests("all good").is_empty());
    }

    #[test]
    fn tail_keeps_only_the_end() {
        let mut t = Tail::default();
        t.write(&vec![b'a'; TAIL_BYTES]);
        t.write(b"END");
        let s = t.string();
        assert_eq!(s.len(), TAIL_BYTES);
        assert!(s.ends_with("END"));
    }

    #[tokio::test]
    async fn a_backgrounded_child_does_not_hold_the_check_open() {
        let dir = tempfile::tempdir().unwrap();
        let argv = vec![
            "bash".into(),
            "-c".into(),
            "sleep 30 & echo started; exit 3".into(),
        ];
        let start = Instant::now();
        let r = run_one(
            "L1",
            "bg",
            &argv,
            dir.path(),
            dir.path(),
            None,
            Duration::from_secs(20),
        )
        .await;
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "took {:?}",
            start.elapsed()
        );
        assert_eq!(r.exit, Some(3));
        assert!(!r.timed_out);
        assert!(r.tail.contains("started"), "{}", r.tail);
    }
}
