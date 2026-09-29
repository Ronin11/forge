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

/// A check command with its execution environment and retained-output limit.
pub struct RunOneCapped<'a> {
    pub level: &'a str,
    pub name: &'a str,
    pub argv: &'a [String],
    pub cwd: &'a Path,
    pub sandbox: Option<&'a Execution>,
    pub timeout: Duration,
    pub env: &'a [(String, String)],
    pub cap_bytes: usize,
    /// Directory a failed check's whole combined stdout and stderr is
    /// written to, whatever its size — `cap_bytes` only bounds what stays
    /// on the row. `None` skips the capture; nothing is written and
    /// `CheckResult::log_path` stays empty.
    pub full_log_dir: Option<&'a Path>,
    /// The egress policy of this one command, when the caller has one of
    /// its own (a job step's declared hosts); the executor unsandboxed
    /// cannot bound a network, so it only travels to a backend that can.
    pub egress: Option<&'a crate::egress::Policy>,
}

use crate::executor::Execution;
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
/// How much of an operation's output the kernel keeps when it asks for the
/// whole thing (`output = "full"` in the action file) rather than the tail.
pub const FULL_OUTPUT_BYTES: usize = 1024 * 1024;
const DRAIN_GRACE: Duration = Duration::from_secs(2);
/// How long a timed-out check has, after SIGTERM to its group, to run its
/// own traps before SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(5);

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
    /// Where the check's whole combined stdout and stderr was written,
    /// when the caller asked for that (`RunOneCapped::full_log_dir`);
    /// empty when it did not, or nothing was written.
    #[serde(default)]
    pub log_path: String,
    /// Stdout alone, same bound as the tail: what an operation that
    /// produces a value hands on. Not serialized with the verdict.
    #[serde(skip)]
    pub stdout: String,
}

/// Keeps the last `cap` bytes written to it.
struct Tail {
    buf: Vec<u8>,
    cap: usize,
}

impl Default for Tail {
    fn default() -> Self {
        Tail::new(TAIL_BYTES)
    }
}

impl Tail {
    fn new(cap: usize) -> Self {
        Tail {
            buf: Vec::new(),
            cap,
        }
    }
    fn write(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
        if self.buf.len() > self.cap {
            let cut = self.buf.len() - self.cap;
            self.buf.drain(..cut);
        }
    }
    fn string(&self) -> String {
        String::from_utf8_lossy(&self.buf).into_owned()
    }
}

async fn drain(
    mut r: impl AsyncReadExt + Unpin,
    tail: Arc<Mutex<Tail>>,
    own: Option<Arc<Mutex<Tail>>>,
    full: Option<Arc<Mutex<Vec<u8>>>>,
) {
    let mut buf = [0u8; 8192];
    while let Ok(n) = r.read(&mut buf).await {
        if n == 0 {
            break;
        }
        tail.lock()
            .unwrap_or_else(|p| p.into_inner())
            .write(&buf[..n]);
        if let Some(o) = &own {
            o.lock().unwrap_or_else(|p| p.into_inner()).write(&buf[..n]);
        }
        if let Some(f) = &full {
            f.lock()
                .unwrap_or_else(|p| p.into_inner())
                .extend_from_slice(&buf[..n]);
        }
    }
}

/// Test names from the formats Forge recognises (go, pytest, jest, cargo);
/// the parser lives in the `forge-test` crate.
pub use forge_test::failing_tests;

/// `env` is added to the command's environment: the task's facts
/// (`operation::task_facts`: `FORGE_TASK_ID`, `FORGE_BASE_SHA`,
/// `FORGE_START_SHA`, `FORGE_BRANCH`) for a check, those plus the
/// operation's own for an operation. A job step's checks pass the job's.
pub async fn run_one(
    level: &str,
    name: &str,
    argv: &[String],
    cwd: &Path,
    sandbox: Option<&Execution>,
    timeout: Duration,
    env: &[(String, String)],
) -> CheckResult {
    run_one_capped(RunOneCapped {
        level,
        name,
        argv,
        cwd,
        sandbox,
        timeout,
        env,
        cap_bytes: TAIL_BYTES,
        full_log_dir: None,
        egress: None,
    })
    .await
}

/// As `run_one`, under `egress`: the policy this command alone is given.
pub async fn run_one_under(
    level: &str,
    name: &str,
    argv: &[String],
    cwd: &Path,
    timeout: Duration,
    env: &[(String, String)],
    egress: &crate::egress::Policy,
) -> CheckResult {
    run_one_capped(RunOneCapped {
        level,
        name,
        argv,
        cwd,
        sandbox: None,
        timeout,
        env,
        cap_bytes: TAIL_BYTES,
        full_log_dir: None,
        egress: Some(egress),
    })
    .await
}

/// Send `signal` to the process group led by `pid`.
async fn signal_group(signal: &str, pid: u32) {
    let _ = Command::new("kill")
        .args([&format!("-{signal}"), "--", &format!("-{pid}")])
        .output()
        .await;
}

/// Write a failed check's whole output under `dir`; the path, if it took.
fn save_full_log(dir: &Path, level: &str, name: &str, bytes: &[u8]) -> Option<String> {
    std::fs::create_dir_all(dir).ok()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let path = dir.join(format!("{level}-{name}-{}-{stamp}.log", std::process::id()));
    std::fs::write(&path, bytes).ok()?;
    Some(path.display().to_string())
}

/// As `run_one`, keeping `cap_bytes` of merged and of stdout-alone output
/// instead of the default tail. An operation with `output = "full"` asks
/// for `FULL_OUTPUT_BYTES` here.
pub async fn run_one_capped(args: RunOneCapped<'_>) -> CheckResult {
    let mut relaunch = crate::agent::Relaunch::default();
    loop {
        let r = run_one_capped_once(&args).await;
        // A check that ran and failed has its stderr merged into `tail`;
        // a launch that bwrap lost never ran the command at all.
        let wall = Duration::from_millis(r.ms as u64);
        if !r.ok && relaunch.again(&r.tail, r.timed_out, wall) {
            eprintln!("{}: {}", args.name, relaunch.note());
            continue;
        }
        return r;
    }
}

/// One launch of `run_one_capped`, without the bwrap relaunch.
async fn run_one_capped_once(args: &RunOneCapped<'_>) -> CheckResult {
    let RunOneCapped {
        level,
        name,
        argv,
        cwd,
        sandbox,
        timeout,
        env,
        cap_bytes,
        full_log_dir,
        egress,
    } = *args;
    let start = Instant::now();
    let mut r = CheckResult {
        level: level.to_string(),
        name: name.to_string(),
        ..Default::default()
    };
    crate::agent::prepare_in(sandbox, cwd, env).await;
    let mut std_cmd = crate::agent::command_under(sandbox, cwd, argv, env, egress);
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
    let tail = Arc::new(Mutex::new(Tail::new(cap_bytes)));
    let stdout = Arc::new(Mutex::new(Tail::new(cap_bytes)));
    // Only allocated when a caller wants the whole output kept; every
    // other check still bounds its capture at `cap_bytes` in `tail`.
    let full = full_log_dir.map(|_| Arc::new(Mutex::new(Vec::<u8>::new())));
    let mut readers = tokio::task::JoinSet::new();
    if let Some(out) = child.stdout.take() {
        readers.spawn(drain(out, tail.clone(), Some(stdout.clone()), full.clone()));
    }
    if let Some(err) = child.stderr.take() {
        readers.spawn(drain(err, tail.clone(), None, full.clone()));
    }

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => {
            r.ok = status.success();
            r.exit = status.code();
        }
        Ok(Err(e)) => r.tail = e.to_string(),
        Err(_) => {
            r.timed_out = true;
            // SIGTERM first, so an operation's EXIT trap can put things
            // back; only what outlives the grace is killed.
            if let Some(pid) = pid {
                signal_group("TERM", pid).await;
            }
            if tokio::time::timeout(TERM_GRACE, child.wait())
                .await
                .is_err()
            {
                child.kill().await.ok();
                child.wait().await.ok();
            }
        }
    }
    // The check has exited; nothing it left behind may outlive it.
    if let Some(pid) = pid {
        signal_group("KILL", pid).await;
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
    r.stdout = stdout.lock().unwrap_or_else(|p| p.into_inner()).string();
    let full_bytes = full.map(|f| f.lock().unwrap_or_else(|p| p.into_inner()).clone());
    // The names come from the whole output when it was kept, not the
    // bounded tail: a failure printed early in a run longer than
    // `cap_bytes` would otherwise never be named.
    let full_text = full_bytes
        .as_deref()
        .map(|b| String::from_utf8_lossy(b).into_owned());
    r.failing_tests = failing_tests(full_text.as_deref().unwrap_or(&text));
    if r.timed_out {
        r.tail = format!("{text}\n[forge] timed out after {}s", timeout.as_secs());
    } else if r.tail.is_empty() {
        r.tail = text;
    }
    if !r.ok {
        // A failed check with nothing to show is a gap in the capture,
        // not a clean run: never leave the record silent about why.
        if r.tail.trim().is_empty() {
            r.tail = format!(
                "[forge] no output captured; the check exited {} with nothing on stdout or stderr",
                r.exit
                    .map_or("with no exit status".to_string(), |c| format!("code {c}"))
            );
        }
        if let (Some(dir), Some(bytes)) = (full_log_dir, &full_bytes) {
            r.log_path = save_full_log(dir, level, name, bytes).unwrap_or_default();
        }
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

    async fn run_sandboxed(failures: u32, argv: &[&str]) -> (CheckResult, u32) {
        let dir = tempfile::tempdir().unwrap();
        let (execution, counter) = crate::agent::fake_bwrap(dir.path(), failures);
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let argv: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
        let r = run_one_capped(RunOneCapped {
            level: "L0",
            name: "sandboxed",
            argv: &argv,
            cwd: &work,
            sandbox: Some(&execution),
            timeout: Duration::from_secs(10),
            env: &[],
            cap_bytes: TAIL_BYTES,
            full_log_dir: None,
            egress: None,
        })
        .await;
        (r, crate::agent::launches(&counter))
    }

    #[tokio::test]
    async fn a_check_relaunches_when_bwrap_loses_the_bind_mount_race() {
        let (r, launches) = run_sandboxed(2, &["/bin/sh", "-c", "echo ran"]).await;
        assert!(r.ok, "{}", r.tail);
        assert!(r.tail.contains("ran"), "{}", r.tail);
        assert_eq!(launches, 3, "two failed launches, then the one that ran");
    }

    #[tokio::test]
    async fn a_check_gives_up_after_three_relaunches() {
        let (r, launches) = run_sandboxed(10, &["/bin/sh", "-c", "echo ran"]).await;
        assert!(!r.ok);
        assert!(r.tail.contains("Can't bind mount"), "{}", r.tail);
        assert_eq!(launches, 4, "the first launch and three relaunches");
    }

    #[tokio::test]
    async fn a_check_that_fails_on_its_own_is_not_relaunched() {
        let (r, launches) = run_sandboxed(0, &["/bin/sh", "-c", "exit 3"]).await;
        assert_eq!(r.exit, Some(3));
        assert_eq!(launches, 1);
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
    async fn a_timed_out_check_gets_sigterm_and_its_trap_runs() {
        let dir = tempfile::tempdir().unwrap();
        let mark = dir.path().join("trap-ran");
        let script = format!(
            "trap 'echo done > {}' EXIT; sleep 30 & wait",
            mark.display()
        );
        let argv = vec!["bash".into(), "-c".into(), script];
        let r = run_one(
            "OP",
            "trap",
            &argv,
            dir.path(),
            None,
            Duration::from_millis(500),
            &[],
        )
        .await;
        assert!(r.timed_out);
        assert!(mark.exists(), "the EXIT trap did not run");
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
            None,
            Duration::from_secs(20),
            &[],
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
        assert_eq!(r.stdout.trim(), "started");
    }

    #[tokio::test]
    async fn stdout_is_kept_apart_from_the_merged_tail_and_env_reaches_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let argv = vec![
            "bash".into(),
            "-c".into(),
            "echo out-$FORGE_X; echo err >&2".into(),
        ];
        let env = vec![("FORGE_X".to_string(), "1".to_string())];
        let r = run_one(
            "OP",
            "e",
            &argv,
            dir.path(),
            None,
            Duration::from_secs(5),
            &env,
        )
        .await;
        assert!(r.ok);
        assert_eq!(r.stdout, "out-1\n");
        assert!(
            r.tail.contains("out-1") && r.tail.contains("err"),
            "{}",
            r.tail
        );
    }

    #[tokio::test]
    async fn a_failed_check_that_prints_10mb_keeps_a_capped_tail_and_the_whole_log() {
        let dir = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        const PAYLOAD: usize = 10 * 1024 * 1024;
        let argv = vec![
            "bash".into(),
            "-c".into(),
            format!(
                "echo '--- FAIL: TestBig (0.00s)'; head -c {PAYLOAD} /dev/zero | tr '\\0' 'a'; exit 1"
            ),
        ];
        let cap_bytes = 4 * 1024;
        let r = run_one_capped(RunOneCapped {
            level: "L1",
            name: "big",
            argv: &argv,
            cwd: dir.path(),
            sandbox: None,
            timeout: Duration::from_secs(30),
            env: &[],
            cap_bytes,
            full_log_dir: Some(logs.path()),
            egress: None,
        })
        .await;
        assert_eq!(r.exit, Some(1));
        assert!(!r.ok);
        assert_eq!(r.failing_tests, vec!["TestBig".to_string()]);
        // Only the last `cap_bytes` is kept on the row, and it is all
        // padding: the failure marker, printed first, is long gone.
        assert!(r.tail.len() <= cap_bytes, "{}", r.tail.len());
        assert!(!r.tail.contains("TestBig"), "{}", r.tail);
        assert!(r.tail.chars().all(|c| c == 'a'), "{}", r.tail);
        // The whole output, unbounded, is on disk at the recorded path.
        assert!(!r.log_path.is_empty());
        let logged = std::fs::read(&r.log_path).unwrap();
        assert_eq!(logged.len(), "--- FAIL: TestBig (0.00s)\n".len() + PAYLOAD);
        assert!(logged.starts_with(b"--- FAIL: TestBig (0.00s)\n"));
        assert!(logged.ends_with(&[b'a'; 100]));
    }
}
