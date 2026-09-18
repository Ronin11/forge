//! Shared e2e test support: a throwaway repo/home/origin harness (`Env`),
//! plus small helpers for driving the real binary and reading its output.

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

pub struct Env {
    pub _dir: tempfile::TempDir,
    pub home: PathBuf,
    pub repo: PathBuf,
    pub origin: PathBuf,
    no_sandbox: bool,
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
        let no_sandbox = std::env::var("FORGE2_TEST_NO_SANDBOX").as_deref() == Ok("1");
        if !no_sandbox {
            let bwrap_ok = Command::new("bwrap")
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(
                bwrap_ok,
                "bwrap is not available; install bubblewrap or set FORGE2_TEST_NO_SANDBOX=1 \
                 to run the e2e suite unsandboxed (sandbox assertions will be skipped)"
            );
        }
        Env {
            _dir: dir,
            home,
            repo,
            origin,
            no_sandbox,
        }
    }

    pub fn cmd(&self, fake: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_forge"));
        c.env("FORGE2_HOME", &self.home);
        c.env(
            "FORGE2_CLAUDE_BIN",
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fakes")
                .join(fake),
        );
        if self.no_sandbox {
            c.env("FORGE2_SANDBOX", "0");
        }
        // The supervisor only runs where a test hands it a fake.
        c.env("FORGE2_SUPERVISOR", "0");
        c
    }

    pub fn sandbox_disabled(&self) -> bool {
        self.no_sandbox
    }

    /// `e.cmd(fake)` with `FORGE2_CLAUDE_BIN_<ROLE>` pointed at `tests/fakes/<role_fake>`.
    pub fn with_role(&self, fake: &str, role: &str, role_fake: &str) -> Command {
        let mut c = self.cmd(fake);
        c.env(
            format!("FORGE2_CLAUDE_BIN_{role}"),
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
