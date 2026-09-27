use crate::support::*;
use std::time::{Duration, Instant};

fn setup_deploy(e: &Env, check: &str) {
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo]
        )
        .status
        .success()
    );
    let dest = format!("dest={}", e._dir.path().join("deploy").display());
    let out = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo,
            "--method",
            "deploy-command",
            "--arg",
            "host=local",
            "--arg",
            &dest,
            "--arg",
            "command=true",
            "--check",
            check,
            "--on-landing",
        ],
    );
    assert!(out.status.success(), "{out:?}");
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for landing");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn landed(e: &Env, id: i64) -> bool {
    e.db().query_row(
        "SELECT landed_sha != '' AND landed_at IS NOT NULL AND reason LIKE 'landed %' FROM tasks WHERE id=?",
        [id], |r| r.get(0),
    ).unwrap_or(false)
}

#[test]
fn a_sleeping_deploy_does_not_hold_the_repository_landing_lock() {
    let e = Env::new();
    let started = e._dir.path().join("started");
    let release = e._dir.path().join("release");
    setup_deploy(
        &e,
        &format!(
            "touch '{}'; while [ ! -f '{}' ]; do sleep 0.1; done",
            started.display(),
            release.display(),
        ),
    );
    let mut first = Worker::spawn(e.cmd("ok.sh").args([
        "run",
        e.repo.to_str().unwrap(),
        "first",
        "--retries",
        "0",
    ]));
    wait_until(|| started.exists());
    assert!(
        landed(&e, 1),
        "landing must be durable before deploy starts"
    );
    let mut second = Worker::spawn(e.cmd("addfile.sh").args([
        "run",
        e.repo.to_str().unwrap(),
        "second",
        "--retries",
        "0",
    ]));
    wait_until(|| landed(&e, 2));
    assert!(!release.exists());
    let unfinished: i64 = e
        .db()
        .query_row(
            "SELECT count(*) FROM deploys WHERE task_id=1 AND finished_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unfinished, 1, "the first deploy is still sleeping");
    std::fs::write(release, "go").unwrap();
    assert!(first.wait().success());
    assert!(second.wait().success());
}

#[test]
fn recovering_a_landing_already_on_the_remote_skips_side_effects() {
    let e = Env::new();
    let calls = e._dir.path().join("deploy-calls");
    setup_deploy(&e, &format!("echo deploy >> '{}'", calls.display()));
    let out = e
        .with_role("ok.sh", "REVIEW", "reviewer-ok.sh")
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "first",
            "--no-land",
            "--workflow",
            "reviewed",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let land = || {
        e.with_role("ok.sh", "ASSESS", "assessor.sh")
            .args(["land", "1"])
            .output()
            .unwrap()
    };
    let out = land();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(std::fs::read_to_string(&calls).unwrap(), "deploy\n");
    // Simulate interruption after the base push but before the task update.
    e.db()
        .execute(
            "UPDATE tasks SET landed_sha='', landed_at=NULL, reason='verified' WHERE id=1",
            [],
        )
        .unwrap();
    let before = op_names(&e, 1);
    let out = land();
    assert!(out.status.success(), "{out:?}");
    assert!(landed(&e, 1));
    assert_eq!(std::fs::read_to_string(calls).unwrap(), "deploy\n");
    assert_eq!(op_names(&e, 1), before, "recovery does not land twice");
    let assessments: i64 = e
        .db()
        .query_row(
            "SELECT count(*) FROM assessments WHERE task_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(assessments, 1, "recovery does not assess twice");
}
