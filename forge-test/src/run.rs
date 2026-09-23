//! `forge-test [--] [command...]`: run the declared test check, or a command,
//! under the repository's timeout with the full log kept outside the worktree,
//! and render a condensed result. `condense` is pure so fixture logs test it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::failing_tests;

const FAILURE_LINES: usize = 15;
const TOTAL_CAP: usize = 60;
const BUILD_TAIL: usize = 20;
const DEFAULT_TIMEOUT_SECS: u64 = 600;

pub const FULL_SUITE_NOTE: &str =
    "Forge runs the full suite after you stop; prefer a filter while iterating";

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal,
    TimedOut(u64),
}

impl Exit {
    pub fn success(self) -> bool {
        self == Exit::Code(0)
    }
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Exit::Code(c) => write!(f, "exit {c}"),
            Exit::Signal => write!(f, "killed by a signal"),
            Exit::TimedOut(s) => write!(f, "timed out after {s}s"),
        }
    }
}

/// The lines of the log belonging to one failing test: from the panic line
/// (or the section header) for at most `FAILURE_LINES` lines.
fn failure_detail(log: &str, name: &str) -> Vec<String> {
    let lines: Vec<&str> = log.lines().collect();
    let header = format!("---- {name} stdout ----");
    let start = match lines.iter().position(|l| l.trim() == header) {
        Some(h) => {
            let body = &lines[h + 1..];
            let end = body
                .iter()
                .position(|l| l.starts_with("---- ") || l.trim() == "failures:")
                .unwrap_or(body.len());
            let body = &body[..end];
            let from = body
                .iter()
                .position(|l| l.contains("panicked at"))
                .unwrap_or(0);
            body[from..].to_vec()
        }
        None => match lines.iter().position(|l| l.contains(name)) {
            Some(i) => lines[i + 1..].to_vec(),
            None => Vec::new(),
        },
    };
    let mut out: Vec<String> = Vec::new();
    for l in start {
        if l.starts_with("note: run with `RUST_BACKTRACE") {
            break;
        }
        if out.is_empty() && l.trim().is_empty() {
            continue;
        }
        out.push(l.trim_end().to_string());
        if out.len() >= FAILURE_LINES {
            break;
        }
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

/// Per-binary totals: each cargo `test result:` line under the most recent
/// `Running`/`Doc-tests` header. Other frameworks' summary lines pass as-is.
fn totals(log: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut suite = String::new();
    for line in log.lines() {
        let t = line.trim();
        if let Some(r) = t.strip_prefix("Running ") {
            suite = r.split(" (").next().unwrap_or(r).to_string();
        } else if let Some(r) = t.strip_prefix("Doc-tests ") {
            suite = format!("doc-tests {r}");
        } else if let Some(r) = t.strip_prefix("test result: ") {
            let r = r.split("; finished in").next().unwrap_or(r);
            if suite.is_empty() {
                out.push(r.to_string());
            } else {
                out.push(format!("{suite}: {r}"));
            }
        }
    }
    out
}

/// The condensed report for one run.
pub fn condense(command: &str, exit: Exit, log: &str, log_path: &str, full_suite: bool) -> String {
    let mut out: Vec<String> = Vec::new();
    if full_suite {
        out.push(FULL_SUITE_NOTE.to_string());
    }
    out.push(format!("$ {command}"));
    out.push(exit.to_string());
    let totals = totals(log);
    let failing = failing_tests(log);
    if !exit.success() && totals.is_empty() && failing.is_empty() {
        out.push(format!("no tests ran; last {BUILD_TAIL} lines:"));
        let all: Vec<&str> = log.lines().collect();
        let from = all.len().saturating_sub(BUILD_TAIL);
        out.extend(all[from..].iter().map(|l| l.to_string()));
    } else {
        out.extend(totals);
        let mut body: Vec<String> = Vec::new();
        for name in &failing {
            body.push(format!("FAILED {name}"));
            body.extend(
                failure_detail(log, name)
                    .into_iter()
                    .map(|l| format!("  {l}")),
            );
        }
        if body.len() > TOTAL_CAP {
            let hidden = body.len() - TOTAL_CAP;
            body.truncate(TOTAL_CAP);
            body.push(format!("... {hidden} more lines in the log"));
        }
        out.extend(body);
    }
    out.push(format!(
        "log: {log_path} (grep this file instead of re-running)"
    ));
    out.join("\n") + "\n"
}

fn read_checks(dir: &Path) -> Result<(Vec<String>, u64), String> {
    let alt = dir.join(".forge/forge.toml");
    let root = dir.join("forge.toml");
    let path = match (alt.exists(), root.exists()) {
        (true, true) => return Err("both .forge/forge.toml and forge.toml exist".into()),
        (true, false) => alt,
        _ => root,
    };
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let doc: toml::Table = text
        .parse()
        .map_err(|e| format!("parsing {}: {e}", path.display()))?;
    let timeout = doc
        .get("defaults")
        .and_then(|d| d.get("check_timeout_secs"))
        .and_then(|t| t.as_integer())
        .map_or(DEFAULT_TIMEOUT_SECS, |t| t.max(1) as u64);
    let argv: Vec<String> = doc
        .get("checks")
        .and_then(|c| c.get("test"))
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if argv.is_empty() {
        return Err(format!("{} declares no [checks].test", path.display()));
    }
    Ok((argv, timeout))
}

fn git_dir(dir: &Path) -> Result<PathBuf, String> {
    let o = Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !o.status.success() {
        return Err("not a git repository".into());
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
}

fn key(argv: &[String]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    argv.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Run and report. Returns the process exit code for `forge-test` itself.
pub fn run(dir: &Path, args: &[String]) -> Result<(String, i32), String> {
    let args = args.strip_prefix(&["--".to_string()]).unwrap_or(args);
    let (argv, timeout, full) = if args.is_empty() {
        let (a, t) = read_checks(dir)?;
        (a, t, true)
    } else {
        let t = read_checks(dir).map_or(DEFAULT_TIMEOUT_SECS, |(_, t)| t);
        (args.to_vec(), t, false)
    };
    let logs = git_dir(dir)?.join("forge-test");
    std::fs::create_dir_all(&logs).map_err(|e| format!("{}: {e}", logs.display()))?;
    let log_path = logs.join(format!("{}.log", key(&argv)));
    let file =
        std::fs::File::create(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?;
    let err = file.try_clone().map_err(|e| e.to_string())?;
    use std::os::unix::process::CommandExt;
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(err)
        .process_group(0)
        .spawn()
        .map_err(|e| format!("{}: {e}", argv[0]))?;
    let start = Instant::now();
    let exit = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break match st.code() {
                Some(c) => Exit::Code(c),
                None => Exit::Signal,
            };
        }
        if start.elapsed() >= Duration::from_secs(timeout) {
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{}", child.id())])
                .status();
            let _ = child.kill();
            let _ = child.wait();
            break Exit::TimedOut(timeout);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let log = String::from_utf8_lossy(&std::fs::read(&log_path).unwrap_or_default()).into_owned();
    let code = match exit {
        Exit::Code(c) => c,
        _ => 1,
    };
    let text = condense(
        &argv.join(" "),
        exit,
        &log,
        &log_path.display().to_string(),
        full,
    );
    Ok((text, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAIL: &str = "\
   Compiling x v0.1.0
     Running unittests src/lib.rs (target/debug/deps/x-abc)

running 2 tests
test a::ok ... ok
test a::bad ... FAILED

failures:

---- a::bad stdout ----

thread 'a::bad' panicked at src/lib.rs:3:5:
assertion `left == right` failed
  left: 1
 right: 2
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    a::bad

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests x
running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
";

    #[test]
    fn failing_run_shows_totals_and_first_panic() {
        let s = condense("cargo test", Exit::Code(101), FAIL, "/g/l.log", false);
        assert!(!s.contains("Forge runs the full suite"));
        assert!(s.contains("$ cargo test\nexit 101\n"));
        assert!(
            s.contains("src/lib.rs: test result")
                || s.contains("src/lib.rs: FAILED. 1 passed; 1 failed")
        );
        assert!(s.contains("doc-tests x: ok. 0 passed"));
        assert!(s.contains("FAILED a::bad\n  thread 'a::bad' panicked at src/lib.rs:3:5:\n  assertion `left == right` failed\n    left: 1"));
        assert!(!s.contains("RUST_BACKTRACE"));
        assert!(s.ends_with("log: /g/l.log (grep this file instead of re-running)\n"));
    }

    #[test]
    fn full_suite_prints_the_note_first() {
        let s = condense("cargo test", Exit::Code(101), FAIL, "/l", true);
        assert_eq!(s.lines().next(), Some(FULL_SUITE_NOTE));
    }

    #[test]
    fn build_failure_shows_the_last_20_lines() {
        let log: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let s = condense("cargo test", Exit::Code(101), &log, "/l", false);
        assert!(s.contains("line 31\n"));
        assert!(!s.contains("line 30\n"));
        assert!(s.contains("line 50\n"));
    }

    #[test]
    fn many_failures_are_capped() {
        let mut log = String::from("     Running unittests a (b)\n");
        for i in 0..30 {
            log += &format!(
                "---- t{i} stdout ----\nthread 't{i}' panicked at a.rs:1:1:\nboom\nx\ny\n\n"
            );
        }
        log += "test result: FAILED. 0 passed; 30 failed; 0 ignored\n";
        let s = condense("c", Exit::Code(101), &log, "/l", false);
        assert!(s.contains("more lines in the log"));
        assert!(s.lines().count() < 70);
    }

    #[test]
    fn success_has_totals_only() {
        let log =
            "     Running unittests a (b)\ntest result: ok. 3 passed; 0 failed; finished in 1s\n";
        let s = condense("c", Exit::Code(0), log, "/l", false);
        assert!(s.contains("unittests a: ok. 3 passed; 0 failed"));
        assert!(!s.contains("FAILED"));
    }

    #[test]
    fn timeout_is_reported() {
        let s = condense("c", Exit::TimedOut(9), "hi\n", "/l", false);
        assert!(s.contains("timed out after 9s"));
    }
}
