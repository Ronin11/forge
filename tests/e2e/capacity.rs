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

fn state(e: &Env, id: i64) -> String {
    e.task(id).0
}

/// Task `id`'s state, or empty while the adoption has not made the store
/// or the row yet.
fn state_if_any(e: &Env, id: i64) -> String {
    e.db()
        .query_row("SELECT state FROM tasks WHERE id=?1", [id], |r| r.get(0))
        .unwrap_or_default()
}

/// `forge adopt` runs its checks in its own process, but on one of its
/// project's slots: with one slot and an adoption verifying, the worker
/// leaves the project's next task queued until the adoption ends.
#[test]
fn an_inline_adoption_holds_a_project_slot_the_worker_respects() {
    let e = Env::new();
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(e.home.join("config.toml"), "[worker]\nslots = 1\n").unwrap();
    // A check held until the test writes .git/gate-open into the clone.
    let toml = std::fs::read_to_string(e.repo.join("forge.toml")).unwrap();
    std::fs::write(
        e.repo.join("forge.toml"),
        format!(
            "{toml}gate = [\"bash\", \"-c\", \"for _ in $(seq 6000); do [ -e .git/gate-open ] && exit 0; sleep 0.1; done; exit 1\"]\n"
        ),
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "a gated check"]);
    git(&e.repo, &["push", "-q", "origin", "main"]);
    git(&e.repo, &["checkout", "-q", "-b", "by-hand", "main"]);
    std::fs::write(e.repo.join("answer.txt"), "42\n").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "by hand"]);
    git(&e.repo, &["checkout", "-q", "main"]);
    let adopt = e
        .cmd("neverrun.sh")
        .args(["adopt", e.repo.to_str().unwrap(), "by-hand", "--no-land"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    assert!(wait_until(
        || state_if_any(&e, 1) == "running",
        scaled(Duration::from_secs(60))
    ));
    let next = e.add(&["--retries", "0"]);
    let worker = Worker::spawn(e.cmd("ok.sh").args(["work", "--poll", "1"]));
    // The worker has had several passes at the queue and kept off it.
    assert!(wait_until(
        || e.home.join("worker.capacity.json").exists(),
        scaled(Duration::from_secs(30))
    ));
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(state(&e, next), "queued");
    let output = e.forge("ok.sh", &["doctor", "--json"]);
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    let row = rows.iter().find(|r| r["name"] == "worker").unwrap();
    let detail = row["detail"].as_str().unwrap();
    assert!(detail.contains("1 of 1 slots"), "{row}");
    assert!(
        detail.contains("task 1 running: forge adopt inline"),
        "{row}"
    );
    let log = e.forge("ok.sh", &["log"]);
    let log = String::from_utf8_lossy(&log.stdout).to_string();
    assert!(
        log.lines()
            .any(|l| l.starts_with("1 ") && l.contains("running")),
        "{log}"
    );
    open_gate(&e, 1);
    let out = adopt.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(wait_until(
        || state(&e, next) != "queued",
        scaled(Duration::from_secs(60))
    ));
    open_gate(&e, next);
    drop(worker);
}
