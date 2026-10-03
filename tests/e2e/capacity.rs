//! Operator capacity and build tuning reach real claims and check processes.
use crate::support::*;
use std::time::Duration;

#[test]
fn capacity_check_sees_operator_build_env_and_repository_override() {
    let e = Env::new();
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        "[sandbox.env]\nCARGO_BUILD_JOBS = '2'\nRUST_TEST_THREADS = '4'\n",
    )
    .unwrap();
    std::fs::write(
        e.repo.join("forge.toml"),
        r#"
[checks]
build-env = ["bash", "-c", "test \"$CARGO_BUILD_JOBS\" = 2 && test \"$RUST_TEST_THREADS\" = 3"]
[sandbox.env]
RUST_TEST_THREADS = "3"
"#,
    )
    .unwrap();
    git(&e.repo, &["add", "forge.toml"]);
    git(
        &e.repo,
        &["commit", "-qm", "check the merged build environment"],
    );
    let output = e.run("ok.sh", &["--retries", "0"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn running(e: &Env) -> i64 {
    e.db()
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE state='running'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn capacity_watcher_and_sighup_resize_without_interrupting_attempts() {
    let e = Env::new();
    std::fs::create_dir_all(&e.home).unwrap();
    let config = e.home.join("config.toml");
    std::fs::write(&config, "[worker]\nslots = 1\n").unwrap();
    for _ in 0..3 {
        e.add(&["--retries", "0"]);
    }
    let worker = Worker::spawn(e.cmd("gated-ok.sh").args(["work", "--poll", "1"]));
    assert!(wait_until(|| running(&e) == 1, Duration::from_secs(30)));
    std::fs::write(&config, "[worker]\nslots = 2\n").unwrap();
    assert!(wait_until(|| running(&e) == 2, Duration::from_secs(30)));
    // The watcher has observed two. SIGHUP forces the next reload too.
    std::fs::write(&config, "[worker]\nslots = 3\n").unwrap();
    let pid: i64 = e
        .db()
        .query_row(
            "SELECT worker_pid FROM tasks WHERE state='running' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        std::process::Command::new("kill")
            .args(["-HUP", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert!(wait_until(|| running(&e) == 3, Duration::from_secs(30)));
    std::fs::write(&config, "[worker]\nslots = 1\n").unwrap();
    assert!(wait_until(
        || {
            std::fs::read_to_string(e.home.join("worker.capacity.json"))
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .is_some_and(|v| v["slots"] == 1)
        },
        Duration::from_secs(30)
    ));
    // Lowering the cap leaves the already-running tasks alive.
    assert_eq!(running(&e), 3);
    let output = e.forge("ok.sh", &["doctor", "--json"]);
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    let row = rows.iter().find(|r| r["name"] == "worker").unwrap();
    assert!(
        row["detail"].as_str().unwrap().contains("3 of 1 slots"),
        "{row}"
    );
    // Worker Drop aborts these gated test processes.
    drop(worker);
}
