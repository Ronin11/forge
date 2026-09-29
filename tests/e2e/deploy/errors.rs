//! An error after a deploy has started (docs/REVIEW-4.md, E3-10) still
//! rolls back and asks, and never overwrites an outcome already recorded.

use super::*;

/// A project `demo` with a `deploy-command` target `prod` whose check is
/// `check`, deploying to a directory on this machine through a fake
/// ssh/rsync, as `a_deploy_that_passes_records_ok_and_a_failing_one_rolls_back_and_blocks_a_question`
/// arranges it. Returns the remote directory and a runner for
/// `forge deploy demo prod --sha <sha>`.
struct Target<'a> {
    e: &'a Env,
    remote: std::path::PathBuf,
    path: String,
    fakehome: std::path::PathBuf,
}

impl<'a> Target<'a> {
    fn new(e: &'a Env, check: &str) -> Target<'a> {
        let repo_s = e.repo.to_str().unwrap();
        assert!(
            e.forge(
                "ok.sh",
                &["project", "new", "demo", "--purpose", "p", "--repo", repo_s],
            )
            .status
            .success()
        );
        let remote = e._dir.path().join("remote");
        let o = e.forge(
            "ok.sh",
            &[
                "project",
                "deploy",
                "add",
                "demo",
                "prod",
                "--repo",
                repo_s,
                "--method",
                "deploy-command",
                "--arg",
                "host=remotebox",
                "--arg",
                &format!("dest={}", remote.display()),
                "--arg",
                "command=true",
                "--check",
                check,
            ],
        );
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let fakebin = e._dir.path().join("fakebin");
        std::fs::create_dir_all(&fakebin).unwrap();
        write_fake_rsync(&fakebin.join("rsync"));
        write_fake(&fakebin.join("ssh"), FAKE_SSH);
        let path = format!(
            "{}:{}",
            fakebin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let fakehome = e._dir.path().join("fakehome");
        std::fs::create_dir_all(&fakehome).unwrap();
        Target {
            e,
            remote,
            path,
            fakehome,
        }
    }

    fn deploy(&self, sha: &str) -> std::process::Output {
        self.e
            .cmd("ok.sh")
            .env("PATH", &self.path)
            .env("HOME", &self.fakehome)
            .args(["deploy", "demo", "prod", "--sha", sha])
            .output()
            .unwrap()
    }

    fn flag(&self) -> String {
        std::fs::read_to_string(self.remote.join("flag.txt")).unwrap()
    }

    fn rows(&self) -> Vec<serde_json::Value> {
        let v: serde_json::Value = serde_json::from_slice(
            &self
                .e
                .forge("ok.sh", &["deploy", "log", "demo", "prod", "--json"])
                .stdout,
        )
        .unwrap();
        v.as_array().unwrap().clone()
    }

    /// The newest task in `demo`: where every deploy question lands.
    fn question(&self) -> (String, String) {
        self.e
            .db()
            .query_row(
                "SELECT state, reason FROM tasks WHERE project = 'demo' ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    /// Record `sha` as a deploy of `prod` that passed its check, as an
    /// earlier on-landing deploy would have.
    fn record_passing(&self, sha: &str) {
        self.e
            .db()
            .execute(
                "INSERT INTO deploys (project, target, sha, started_at, finished_at, check_ok, task_id)
                 VALUES ('demo', 'prod', ?1, 1, 2, 1, 1)",
                [sha],
            )
            .unwrap();
    }
}

fn commit_flag(repo: &Path, flag: &str) -> String {
    std::fs::write(repo.join("flag.txt"), format!("{flag}\n")).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", flag]);
    git(repo, &["rev-parse", "HEAD"])
}

/// A smoke step that cannot even run (here: its output directory cannot be
/// created, since a file already sits where it would go) is a failed smoke
/// result, not an error: the deploy rolls back to the last passing commit
/// and asks. `FORGE_HOME/deploys` itself stays a real directory now that
/// deploy::run's per-target lock lives there too.
#[test]
fn a_smoke_step_that_errors_rolls_back_and_asks() {
    let e = Env::new();
    let t = Target::new(&e, "true");
    let good = commit_flag(&e.repo, "good");
    let o = t.deploy(&good);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "set",
            "demo",
            "prod",
            "--smoke",
            "http://127.0.0.1:1/",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // The next deploy row is id 2 (the only prior row, `good`, is id 1): put
    // a file where its smoke output directory would go, so only that
    // `mkdir` fails.
    std::fs::create_dir_all(e.home.join("deploys")).unwrap();
    std::fs::write(e.home.join("deploys").join("2"), "not a directory").unwrap();

    let bad = commit_flag(&e.repo, "bad");
    let o = t.deploy(&bad);
    assert!(!o.status.success());
    let rows = t.rows();
    let failed = &rows[0];
    assert_eq!(failed["sha"], bad, "{failed:?}");
    assert_eq!(failed["check_ok"], false, "{failed:?}");
    assert_eq!(failed["smoke_ok"], false, "{failed:?}");
    assert_eq!(failed["rolled_back_to"], good, "{failed:?}");
    let output = failed["check_output"].as_str().unwrap();
    assert!(output.contains("smoke step could not run"), "{output}");
    assert_eq!(t.flag(), "good\n");

    let (state, reason) = t.question();
    assert_eq!(state, "blocked");
    assert!(reason.contains("rolled back to"), "{reason}");
    assert!(reason.contains("smoke step could not run"), "{reason}");
}

/// An operator's `forge deploy` whose previous passing deploy was an
/// on-landing one, of a commit staged only in the kernel repository: the
/// rollback archives it from there rather than failing on the registered
/// checkout that lacks it.
#[test]
fn a_rollback_to_a_commit_only_the_kernel_repository_has_archives_it_from_there() {
    let e = Env::new();
    let t = Target::new(&e, "grep -qx good flag.txt");

    // The good commit exists in another clone and the kernel repository,
    // never in the registered checkout.
    let other = e._dir.path().join("other");
    git(
        e._dir.path(),
        &[
            "clone",
            "-q",
            e.repo.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    git(&other, &["config", "user.email", "t@example.com"]);
    git(&other, &["config", "user.name", "t"]);
    let good = commit_flag(&other, "good");
    let repo: String = e
        .db()
        .query_row(
            "SELECT repo FROM deploy_targets WHERE name='prod'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let key = {
        use std::io::Write;
        let mut c = std::process::Command::new("sha256sum")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(repo.as_bytes()).unwrap();
        let out = String::from_utf8(c.wait_with_output().unwrap().stdout).unwrap();
        out.split_whitespace().next().unwrap().to_string()
    };
    let kernel = e.home.join("repositories").join(format!("{key}.git"));
    if !kernel.exists() {
        std::fs::create_dir_all(&kernel).unwrap();
        git(&kernel, &["init", "--bare", "-q"]);
    }
    git(
        &kernel,
        &[
            "fetch",
            "-q",
            other.to_str().unwrap(),
            "HEAD:refs/forge/landed",
        ],
    );
    let lacks = std::process::Command::new("git")
        .arg("-C")
        .arg(&e.repo)
        .args(["cat-file", "-e", &format!("{good}^{{commit}}")])
        .status()
        .unwrap();
    assert!(!lacks.success(), "the registered checkout must lack {good}");
    t.record_passing(&good);

    let bad = commit_flag(&e.repo, "bad");
    let o = t.deploy(&bad);
    assert!(!o.status.success());
    let failed = &t.rows()[0];
    assert_eq!(failed["sha"], bad, "{failed:?}");
    assert_eq!(failed["check_ok"], false, "{failed:?}");
    assert_eq!(failed["rolled_back_to"], good, "{failed:?}");
    assert_eq!(t.flag(), "good\n");
    let (state, reason) = t.question();
    assert_eq!(state, "blocked");
    assert!(reason.contains("rolled back to"), "{reason}");
}

/// An error before any outcome was recorded (here: the rollback's commit
/// exists nowhere, so there is nothing to archive) finishes the row with
/// the error and files the question itself, since nothing else asked.
#[test]
fn an_error_before_a_rollback_is_recorded_finishes_the_row_and_asks() {
    let e = Env::new();
    let t = Target::new(&e, "grep -qx good flag.txt");
    let ghost = "0123456789abcdef0123456789abcdef01234567";
    t.record_passing(ghost);

    let bad = commit_flag(&e.repo, "bad");
    let o = t.deploy(&bad);
    assert!(!o.status.success());
    let failed = &t.rows()[0];
    assert_eq!(failed["sha"], bad, "{failed:?}");
    assert_eq!(failed["check_ok"], false, "{failed:?}");
    assert!(!failed["finished_at"].is_null(), "{failed:?}");
    assert_eq!(failed["rolled_back_to"], serde_json::Value::Null);
    let why = failed["reason"].as_str().unwrap();
    assert!(why.contains("01234567"), "{why}");

    let (state, reason) = t.question();
    assert_eq!(state, "blocked");
    assert!(reason.contains("not rolled back"), "{reason}");
    assert!(reason.contains(&bad[..8]), "{reason}");

    let (id, recipient): (i64, Option<String>) = e
        .db()
        .query_row(
            "SELECT id, question_to FROM tasks WHERE deploy_id=?1",
            [failed["id"].as_i64().unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(recipient, None, "the question is for the operator");
    let rows = crate::audience::events(&e);
    let done: Vec<_> = rows
        .iter()
        .filter(|v| v["type"] == "task_done" && v["task"] == id)
        .collect();
    assert_eq!(done.len(), 1, "the deploy question is announced once");
    assert_eq!(done[0]["audience"], "person");
    assert_eq!(done[0]["state"], "blocked");
    assert_eq!(done[0]["reason"], reason);
    let hits = crate::audience::notify(&e, &rows, "", &e._dir.path().join("notify"));
    assert_eq!(hits.len(), 1, "the operator receives one message: {hits:?}");
    assert!(hits[0].starts_with(&format!("{id}|blocked|needs input:")));
}
