//! Shared e2e test support: a throwaway repo/home/origin harness (`Env`),
//! plus small helpers for driving the real binary and reading its output.

use rusqlite::Connection;
use std::cell::RefCell;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output};
use std::time::{Duration, Instant};

pub struct Env {
    pub _dir: tempfile::TempDir,
    pub home: PathBuf,
    pub repo: PathBuf,
    pub origin: PathBuf,
    /// `XDG_CONFIG_HOME` for every spawned `forge`, so `forge init`'s
    /// systemd units land under the test's own tempdir rather than the
    /// machine running the suite's real `~/.config/systemd/user`.
    pub xdg_config: PathBuf,
    no_sandbox: bool,
}

/// A spawned `forge work` (or other long-lived process a test drives by
/// hand, a plugin run directly included): SIGTERM, then SIGKILL if it
/// outlives a grace period, and reaped on drop. Every test that spawns a
/// worker should hold one of these rather than a bare `Child`, so a failing
/// assertion between spawn and an explicit stop can never leave the process
/// running past the test (five idle `forge work` processes were once found
/// still running after an e2e binary had already exited, one per test that
/// only stopped its worker on the success path).
///
/// The process leads its own process group, and stopping it signals the
/// group, so a shell plugin's pipeline dies with its shell. Whatever the
/// process had forked (the plugins a worker supervises sit in groups of
/// their own) is remembered as the test signals it, and once the process
/// is gone none of it may survive: a survivor is killed and reported.
pub struct Worker {
    child: Option<Child>,
    seen: RefCell<Vec<Proc>>,
}

/// A process identified by pid and start time, so a recycled pid is not
/// mistaken for the process that used to hold it.
#[derive(Clone, PartialEq)]
struct Proc {
    pid: i32,
    started: String,
}

/// `(pid, ppid, state, start time)` for every process in `/proc`.
fn process_table() -> Vec<(i32, i32, char, String)> {
    let mut rows = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return rows;
    };
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `pid (comm) S ppid ...`: comm may hold spaces and parentheses, so
        // the fields are counted from the last `)`.
        let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else {
            continue;
        };
        let f: Vec<&str> = rest.split_whitespace().collect();
        if f.len() > 19 {
            let state = f[0].chars().next().unwrap_or('?');
            rows.push((pid, f[1].parse().unwrap_or(0), state, f[19].to_string()));
        }
    }
    rows
}

/// Every live descendant of `root` right now.
fn descendants(root: i32) -> Vec<Proc> {
    let table = process_table();
    let mut found: Vec<i32> = vec![root];
    let mut out = Vec::new();
    let mut i = 0;
    while i < found.len() {
        let parent = found[i];
        i += 1;
        for (pid, ppid, state, started) in &table {
            if *ppid == parent && *state != 'Z' && !found.contains(pid) {
                found.push(*pid);
                out.push(Proc {
                    pid: *pid,
                    started: started.clone(),
                });
            }
        }
    }
    out
}

/// True while `p` is still running (a zombie awaiting its reaper is gone).
fn alive(p: &Proc) -> bool {
    process_table()
        .iter()
        .any(|(pid, _, state, started)| *pid == p.pid && *state != 'Z' && *started == p.started)
}

impl Worker {
    pub fn spawn(cmd: &mut Command) -> Worker {
        cmd.process_group(0);
        Worker {
            child: Some(cmd.spawn().expect("spawn worker")),
            seen: RefCell::new(Vec::new()),
        }
    }

    pub fn id(&self) -> u32 {
        self.child.as_ref().expect("worker already taken").id()
    }

    /// Remembers what the worker has forked so far.
    fn note_children(&self) {
        let mut seen = self.seen.borrow_mut();
        for p in descendants(self.id() as i32) {
            if !seen.contains(&p) {
                seen.push(p);
            }
        }
    }

    pub fn signal(&self, sig: libc::c_int) {
        self.note_children();
        unsafe {
            libc::kill(self.id() as i32, sig);
        }
    }

    pub fn wait(&mut self) -> ExitStatus {
        let status = self
            .child
            .as_mut()
            .expect("worker already taken")
            .wait()
            .expect("wait worker");
        self.reap_group();
        self.assert_no_survivors();
        status
    }

    /// SIGTERM and wait for a clean exit: the shape most tests want.
    pub fn stop(&mut self) -> ExitStatus {
        self.signal(libc::SIGTERM);
        self.wait()
    }

    /// SIGTERM and wait for a clean exit, returning captured output.
    pub fn stop_with_output(mut self) -> Output {
        self.signal(libc::SIGTERM);
        let out = self
            .child
            .take()
            .expect("worker already taken")
            .wait_with_output()
            .expect("wait worker");
        self.reap_group();
        self.assert_no_survivors();
        out
    }

    /// SIGKILL the worker to simulate a crash and reap it. A crashed worker
    /// cannot stop the operation steps it was running (only its plugins are
    /// tied to its life, by the parent-death signal), so those are killed
    /// here rather than reported as survivors.
    pub fn crash(&mut self) -> ExitStatus {
        self.signal(libc::SIGKILL);
        let status = self
            .child
            .as_mut()
            .expect("worker already taken")
            .wait()
            .expect("wait worker");
        for p in self.seen.take().iter().filter(|p| alive(p)) {
            unsafe {
                libc::kill(p.pid, libc::SIGKILL);
            }
        }
        self.reap_group();
        status
    }

    /// SIGKILL whatever is left of the worker's own process group (a shell
    /// plugin's pipeline, once its shell has gone).
    fn reap_group(&self) {
        if let Some(child) = &self.child {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
        }
    }

    /// Test-only check that nothing the worker forked outlives it, given two
    /// seconds for the kernel to deliver the parent-death signals. Anything
    /// still running is killed, then reported.
    fn assert_no_survivors(&self) {
        let seen = self.seen.take();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut left: Vec<Proc> = seen.into_iter().filter(alive).collect();
        while !left.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
            left.retain(alive);
        }
        for p in &left {
            unsafe {
                libc::kill(p.pid, libc::SIGKILL);
            }
        }
        if !left.is_empty() && !std::thread::panicking() {
            let pids: Vec<i32> = left.iter().map(|p| p.pid).collect();
            panic!("processes the worker started outlived it: {pids:?}");
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        let pid = child.id() as i32;
        let exited = matches!(child.try_wait(), Ok(Some(_)));
        if !exited {
            self.note_children();
            let child = self.child.as_mut().expect("checked above");
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
        self.reap_group();
        self.assert_no_survivors();
    }
}

/// True if `dir` does not exist, or exists but holds no regular file
/// anywhere under it — an empty subdirectory doesn't count as a file.
/// What `forge workflows lint --stdin` must leave a fresh `FORGE_HOME`,
/// unlike every other catalog command, which writes the built-ins into it.
pub fn holds_no_files(dir: &Path) -> bool {
    fn walk(dir: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return true;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !walk(&path) {
                    return false;
                }
            } else {
                return false;
            }
        }
        true
    }
    !dir.exists() || walk(dir)
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        o.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

impl Env {
    pub fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let repo = dir.path().join("repo");
        let origin = dir.path().join("origin.git");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        std::fs::write(
            repo.join("forge.toml"),
            "[checks]\nanswer = [\"bash\", \"-c\", \"test -f answer.txt && grep -qx 42 answer.txt\"]\nshell = [\"bash\", \"-n\", \"hello.sh\"]\n",
        )
        .unwrap();
        std::fs::write(repo.join("hello.sh"), "#!/bin/bash\necho hello\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);
        Command::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&origin)
            .status()
            .unwrap();
        git(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        let no_sandbox = std::env::var("FORGE_TEST_NO_SANDBOX").as_deref() == Ok("1");
        if !no_sandbox {
            let bwrap_ok = Command::new("bwrap")
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(
                bwrap_ok,
                "bwrap is not available; install bubblewrap or set FORGE_TEST_NO_SANDBOX=1 \
                 to run the e2e suite unsandboxed (sandbox assertions will be skipped)"
            );
        }
        let xdg_config = dir.path().join("xdg_config");
        Env {
            _dir: dir,
            home,
            repo,
            origin,
            xdg_config,
            no_sandbox,
        }
    }

    pub fn cmd(&self, fake: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_forge"));
        c.env("FORGE_HOME", &self.home);
        c.env("XDG_CONFIG_HOME", &self.xdg_config);
        // Fixture homes may live on a tmpfs smaller than the production
        // disk reserve. Disk-specific tests remove or override this value.
        c.env("FORGE_MIN_FREE_GB", "0");
        c.env(
            "FORGE_CLAUDE_BIN",
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fakes")
                .join(fake),
        );
        if self.no_sandbox {
            c.env("FORGE_SANDBOX", "0");
        }
        // The supervisor only runs where a test hands it a fake.
        c.env("FORGE_SUPERVISOR", "0");
        c
    }

    pub fn sandbox_disabled(&self) -> bool {
        self.no_sandbox
    }

    /// `e.cmd(fake)` with `FORGE_CLAUDE_BIN_<ROLE>` pointed at `tests/fakes/<role_fake>`.
    pub fn with_role(&self, fake: &str, role: &str, role_fake: &str) -> Command {
        let mut c = self.cmd(fake);
        c.env(
            format!("FORGE_CLAUDE_BIN_{role}"),
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fakes")
                .join(role_fake),
        );
        c
    }

    pub fn forge(&self, fake: &str, args: &[&str]) -> Output {
        let o = self.cmd(fake).args(args).output().expect("forge");
        eprintln!(
            "--- forge {} ---\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        o
    }

    /// `forge <args>` with `stdin` piped to the process, for
    /// `forge workflows lint --stdin`.
    pub fn forge_stdin(&self, fake: &str, args: &[&str], stdin: &str) -> Output {
        use std::io::Write as _;
        use std::process::Stdio;
        let mut child = self
            .cmd(fake)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn forge");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let o = child.wait_with_output().expect("forge");
        eprintln!(
            "--- forge {} <stdin ---\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        o
    }

    pub fn trace_json(&self, id: impl std::fmt::Display) -> serde_json::Value {
        serde_json::from_slice(
            &self
                .forge("ok.sh", &["trace", &id.to_string(), "--json"])
                .stdout,
        )
        .unwrap()
    }

    pub fn requests_json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.forge("ok.sh", &["requests", "--json"]).stdout).unwrap()
    }

    pub fn decisions_json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.forge("ok.sh", &["decisions", "--json"]).stdout).unwrap()
    }

    pub fn run(&self, fake: &str, extra: &[&str]) -> Output {
        let mut args = vec![
            "run",
            self.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--no-land",
        ];
        args.extend_from_slice(extra);
        self.forge(fake, &args)
    }

    pub fn add(&self, extra: &[&str]) -> i64 {
        let mut args = vec!["add", self.repo.to_str().unwrap(), "write 42 to answer.txt"];
        args.extend_from_slice(extra);
        let o = self.forge("ok.sh", &args);
        assert!(o.status.success());
        String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .nth(2)
            .unwrap()
            .parse()
            .unwrap()
    }

    pub fn db(&self) -> Connection {
        Connection::open(self.home.join("forge.db")).unwrap()
    }

    pub fn task(&self, id: i64) -> (String, String, bool) {
        self.db()
            .query_row(
                "SELECT state, reason, pushed FROM tasks WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)),
            )
            .unwrap()
    }

    /// (attempt_no, state, reason, timed_out, verdict_json)
    pub fn attempts(&self, id: i64) -> Vec<(i64, String, String, bool, String)> {
        let c = self.db();
        let mut s = c
            .prepare("SELECT attempt_no, state, reason, timed_out, verdict_json FROM attempts WHERE task_id=?1 ORDER BY attempt_no")
            .unwrap();
        s.query_map([id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get::<_, i64>(3)? != 0,
                r.get(4)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    pub fn origin_branches(&self) -> String {
        git(&self.origin, &["branch"])
    }

    pub fn log_text(&self, task: i64, attempt: i64) -> String {
        std::fs::read_to_string(
            self.home
                .join("logs")
                .join(format!("{task}-{attempt}.jsonl")),
        )
        .unwrap()
    }
}

pub fn check(verdict: &str, level: &str, name: &str) -> Option<bool> {
    let v: Vec<serde_json::Value> = serde_json::from_str(verdict).unwrap();
    v.iter()
        .find(|c| c["level"] == level && c["name"] == name)
        .map(|c| c["ok"].as_bool().unwrap())
}

pub fn wait_until(pred: impl Fn() -> bool, timeout: Duration) -> bool {
    let t0 = Instant::now();
    loop {
        if pred() {
            return true;
        }
        if t0.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn tdd_repo(e: &Env) {
    let test_cmd = "shopt -s nullglob; n=0; for f in tests/acceptance/*.sh; do n=$((n+1)); bash \"$f\" || exit 1; done; test $n -gt 0";
    std::fs::write(
        e.repo.join("forge.toml"),
        format!(
            "[checks]\nshell = [\"bash\", \"-n\", \"hello.sh\"]\ntest = [\"bash\", \"-c\", {}]\n[verify]\nnamespace = [\"tests/acceptance/\"]\n",
            serde_json::to_string(test_cmd).unwrap()
        ),
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "tdd layout"]);
}

/// A repo whose `fmt` check only ever passes once `fmt.txt` says `GOOD`,
/// with a fix command declared for it that rewrites `fmt.txt` to say so:
/// the fixture for the deterministic known-fixes step, the way `tdd_repo`
/// is the fixture for the tests contract.
pub fn fixable_repo(e: &Env) {
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\n\
         answer = [\"bash\", \"-c\", \"test -f answer.txt && grep -qx 42 answer.txt\"]\n\
         fmt = [\"bash\", \"-c\", \"grep -qx GOOD fmt.txt\"]\n\
         \n\
         [checks.fixable]\n\
         fmt = [\"bash\", \"fix-fmt.sh\"]\n",
    )
    .unwrap();
    std::fs::write(e.repo.join("fmt.txt"), "BAD\n").unwrap();
    std::fs::write(
        e.repo.join("fix-fmt.sh"),
        "#!/bin/bash\necho GOOD > fmt.txt\n",
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "a fixable fmt check"]);
}

pub fn run_tdd(e: &Env, coder: &str, writer: &str, task: &str) -> Output {
    let mut c = e.with_role(coder, "TESTS", writer);
    let o = c
        .args([
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            task,
            "--workflow",
            "tdd",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    eprintln!(
        "--- tdd {coder}+{writer} ---\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

pub fn run_wf(
    e: &Env,
    coder: &str,
    extra_env: &[(&str, &str)],
    workflow: &str,
    task: &str,
) -> Output {
    let mut c = e.cmd(coder);
    for (k, v) in extra_env {
        c.env(
            k,
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fakes")
                .join(v),
        );
    }
    let o = c
        .args([
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            task,
            "--workflow",
            workflow,
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    eprintln!(
        "--- {workflow} {coder} ---\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

pub fn origin_file(e: &Env, branch: &str, path: &str) -> Option<String> {
    let o = Command::new("git")
        .args([
            "--git-dir",
            e.origin.to_str().unwrap(),
            "show",
            &format!("{branch}:{path}"),
        ])
        .output()
        .unwrap();
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).to_string())
}

pub fn op_names(e: &Env, id: i64) -> Vec<(String, bool)> {
    let doc: serde_json::Value = e.trace_json(id);
    doc["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            (
                o["name"].as_str().unwrap().to_string(),
                o["ok"].as_bool().unwrap(),
            )
        })
        .collect()
}

/// Commit whatever the test changed in the operator catalog as the
/// operator: only such a commit lets a copy of a built-in action win over
/// the built-in (docs/WORKFLOWS.md, "Authoring").
pub fn commit_catalog_edit(e: &Env) {
    let cat = e.home.join("workflows");
    git(&cat, &["add", "-A"]);
    git(
        &cat,
        &[
            "-c",
            "user.name=operator",
            "-c",
            "user.email=operator@localhost",
            "commit",
            "-qm",
            "operator edit",
        ],
    );
}
