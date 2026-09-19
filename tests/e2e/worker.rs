use crate::support::*;
use std::time::{Duration, Instant};

#[test]
fn doctor_runs_and_reports_the_essentials() {
    let e = Env::new();
    assert!(e.run("ok.sh", &[]).status.success());
    std::fs::write(
        e.home.join("worker.pid"),
        format!("{} /bin/true\n", std::process::id()),
    )
    .unwrap();
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");
    for name in [
        "binary.git",
        "sandbox",
        "home",
        "config",
        "schema",
        "purposes",
        "queue",
        "worktrees",
        "logs",
        "spend",
        "rate_limit",
        "worker",
        "cache",
    ] {
        assert!(out.contains(name), "missing {name} in:\n{out}");
    }
    assert!(out.contains("5h 42%"), "{out}");
    assert!(
        out.contains("events.jsonl") && out.contains("attempt log"),
        "{out}"
    );
    assert!(out.contains("OK   cache"), "{out}");
    assert!(out.contains("blob file"), "{out}");
    // exercised: `forge run` above ran the repo-map step, so the shared
    // cache under FORGE2_HOME/cache/repomap holds at least one blob.
    assert!(!out.contains("WARN cache"), "{out}");

    let o = e.forge("ok.sh", &["doctor", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    let checks: Vec<serde_json::Value> =
        serde_json::from_slice(&o.stdout).expect("doctor --json prints a parseable JSON array");
    assert!(
        checks
            .iter()
            .any(|c| c["name"] == "worker" && c["status"] == "ok"),
        "missing worker row in {checks:?}"
    );
    assert!(
        checks.iter().any(|c| c["name"] == "cache"
            && c["status"] == "ok"
            && c["detail"].as_str().unwrap().contains("blob file")),
        "missing cache row in {checks:?}"
    );
    for field in ["name", "status", "detail", "hint"] {
        assert!(
            checks.iter().all(|c| c.get(field).is_some()),
            "every check should have {field} in {checks:?}"
        );
    }
}

#[test]
fn an_attempt_runs_sandboxed_when_bwrap_is_present() {
    let e = Env::new();
    if e.sandbox_disabled() {
        eprintln!("FORGE2_TEST_NO_SANDBOX=1: skipping, bwrap unavailable");
        return;
    }
    let o = e.run("ok.sh", &[]);
    assert!(o.status.success());
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(out.contains("sandboxed"), "{out}");
}

#[test]
fn doctor_warns_when_the_repomap_cache_is_missing() {
    let e = Env::new();
    // Bootstraps FORGE2_HOME without ever running a task, so
    // cache/repomap is never created.
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("WARN cache"), "{out}");
    assert!(out.contains("does not exist"), "{out}");
}

#[test]
fn doctor_warns_for_a_project_with_the_migrations_placeholder_purpose() {
    let e = Env::new();
    // `e.add` queues a task with no project named, so `ensure_default_project`
    // creates one with the migration's placeholder purpose.
    e.add(&[]);
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("WARN purposes"), "{out}");
    assert!(out.contains("repo"), "{out}");
    assert!(out.contains("forge project set"), "{out}");

    // `forge project set --purpose` clears it.
    assert!(
        e.forge(
            "ok.sh",
            &["project", "set", "repo", "--purpose", "A real purpose."]
        )
        .status
        .success()
    );
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("OK   purposes"), "{out}");
}

#[test]
fn doctor_warns_when_attempt_logs_pass_a_gigabyte() {
    let e = Env::new();
    assert!(e.run("ok.sh", &[]).status.success());
    let big = e.home.join("logs").join("999-1.jsonl");
    std::fs::File::create(&big)
        .unwrap()
        .set_len(1_100_000_000)
        .unwrap();
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("WARN logs"), "{out}");
    assert!(out.contains("archive or delete old attempt logs"), "{out}");
}

#[test]
fn a_hanging_agent_is_killed_at_the_timeout() {
    let e = Env::new();
    let start = Instant::now();
    assert!(
        !e.run("hang.sh", &["--retries", "0", "--timeout-secs", "2"])
            .status
            .success()
    );
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "took {:?}",
        start.elapsed()
    );
    let a = e.attempts(1);
    assert!(a[0].3, "timed_out");
    assert_eq!(a[0].2, "agent timed out");
    assert_eq!(e.task(1).0, "failed");
}

#[test]
fn a_crashing_agent_is_retried_then_fails() {
    let e = Env::new();
    assert!(!e.run("crash.sh", &["--retries", "1"]).status.success());
    let a = e.attempts(1);
    assert_eq!(a.len(), 2);
    assert!(
        a.iter()
            .all(|x| x.1 == "agent_failed" && x.2 == "agent exit 1")
    );
    assert!(e.task(1).1.starts_with("agent exit 1"));
}

#[test]
fn an_orphaned_task_is_requeued_and_resumes_at_the_next_attempt() {
    let e = Env::new();
    let id = e.add(&[]);
    let c = e.db();
    c.execute(
        "UPDATE tasks SET state='running', worker_pid=999999999 WHERE id=?1",
        [id],
    )
    .unwrap();
    c.execute(
        "INSERT INTO attempts(task_id, attempt_no, state, started_at) VALUES (?1, 1, 'running', 0)",
        [id],
    )
    .unwrap();
    assert!(e.forge("ok.sh", &["work", "--once"]).status.success());
    let a = e.attempts(id);
    assert_eq!(a[0].1, "agent_failed");
    assert_eq!(a[0].2, "previous worker exited");
    assert_eq!(a[1].0, 2);
    assert_eq!(a[1].1, "succeeded");
}

#[test]
fn an_environment_fault_requeues_and_stops_the_worker() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    e.add(&[]);
    e.add(&[]);
    // The attempt's log cannot be created: that is the worker's problem, not the task's.
    let logs = e.home.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o555)).unwrap();
    let o = e.forge("ok.sh", &["work", "--once"]);
    std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("back in the queue"));
    assert_eq!(e.task(1).0, "queued");
    assert!(
        e.task(1).1.contains("worker environment error"),
        "{}",
        e.task(1).1
    );
    assert_eq!(
        e.task(2).0,
        "queued",
        "the worker must stop, not fail the rest"
    );
    assert_eq!(e.attempts(1)[0].2, "worker environment error");
}

#[test]
fn a_missing_agent_binary_fails_fast_with_nothing_claimed() {
    let e = Env::new();
    e.add(&[]);
    let o = e.forge("does-not-exist.sh", &["work", "--once"]);
    assert!(!o.status.success());
    assert_eq!(e.task(1).0, "queued");
    assert_eq!(e.attempts(1).len(), 0);
}

#[test]
fn jobs_run_in_parallel() {
    let e = Env::new();
    for _ in 0..3 {
        e.add(&[]);
    }
    let start = Instant::now();
    assert!(
        e.cmd("ok.sh")
            .env("FAKE_SLEEP", "1")
            .args(["work", "--once", "--jobs", "3"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        start.elapsed() < Duration::from_secs(60),
        "took {:?}",
        start.elapsed()
    );
    for id in 1..=3 {
        assert_eq!(e.task(id).0, "succeeded");
    }
}

#[test]
fn a_second_signal_aborts_and_requeues() {
    let e = Env::new();
    let id = e.add(&[]);
    std::fs::create_dir_all(&e.home).unwrap();
    let stderr_path = e.home.join("worker-stderr.log");
    let stderr_file = std::fs::File::create(&stderr_path).unwrap();
    let mut worker = Worker::spawn(
        e.cmd("hang.sh")
            .args(["work", "--once"])
            .stderr(stderr_file),
    );
    assert!(
        wait_until(
            || e.attempts(id).first().is_some_and(|a| a.1 == "running"),
            Duration::from_secs(20)
        ),
        "the worker never claimed the task"
    );
    worker.signal(libc::SIGINT);
    assert!(
        wait_until(
            || std::fs::read_to_string(&stderr_path)
                .map(|s| s.contains("stopping: no new tasks"))
                .unwrap_or(false),
            Duration::from_secs(20)
        ),
        "the worker never acknowledged the first signal"
    );
    worker.signal(libc::SIGINT);
    let status = worker.wait();
    assert!(status.success());
    let (state, reason, _) = e.task(id);
    assert_eq!(state, "queued");
    assert!(reason.contains("aborted"), "{reason}");
    assert_eq!(e.attempts(id)[0].2, "worker aborted by operator");
}

#[test]
fn a_sigterm_drains_the_running_attempt_and_exits_cleanly() {
    let e = Env::new();
    let id = e.add(&[]);
    let worker = Worker::spawn(
        e.cmd("ok.sh")
            .env("FAKE_SLEEP", "1")
            .args(["work", "--once"])
            .stderr(std::process::Stdio::piped()),
    );
    // ok.sh with FAKE_SLEEP set takes 2s to answer; signal once the worker has claimed the
    // task so the drain has real work to wait out, not a race with an attempt already done.
    assert!(
        wait_until(|| e.task(id).0 == "running", Duration::from_secs(10)),
        "the worker claimed the task"
    );
    let o = worker.stop_with_output();
    let err = String::from_utf8_lossy(&o.stderr);
    eprintln!("--- sigterm drain ---\n{err}");
    assert!(o.status.success(), "{err}");
    assert!(
        err.contains("running attempt(s) will finish"),
        "the worker announced the drain: {err}"
    );
    assert_eq!(e.task(id).0, "succeeded");
    assert_eq!(e.attempts(id)[0].1, "succeeded");
}

#[test]
fn gc_removes_only_what_is_published_and_clean() {
    let e = Env::new();
    assert!(e.run("ok.sh", &[]).status.success());
    assert!(!e.run("wrong.sh", &["--retries", "0"]).status.success());
    let o = e.forge("ok.sh", &["gc"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("task 1    removed"), "{out}");
    assert!(
        out.contains("task 2    kept (1 commit(s) not on the remote)"),
        "{out}"
    );
    assert!(!e.home.join("worktrees/1").exists());
    assert!(e.home.join("worktrees/2").exists());
    assert!(
        e.origin_branches().contains("forge/1-"),
        "published branches are never deleted"
    );
    assert!(!e.home.join("worktrees/1").exists());
}

#[test]
fn gc_treats_a_blocked_task_superseded_by_a_later_success_like_a_failed_one() {
    let e = Env::new();
    // Task 1 commits an answer, then blocks on a question instead of finishing.
    assert!(
        !e.run("commitneedsinput.sh", &["--retries", "2"])
            .status
            .success()
    );
    let (state, _, pushed) = e.task(1);
    assert_eq!(state, "blocked");
    assert!(!pushed);
    // Task 2 retries and succeeds, superseding task 1's unpublished commit.
    // Task 1's checks already passed, so task 2 starts from its branch
    // (verified_branch_of): the coder here must add something of its own
    // rather than repeat task 1's now-already-committed write.
    assert!(e.forge("ok.sh", &["retry", "1"]).status.success());
    assert!(e.forge("addfile.sh", &["work", "--once"]).status.success());
    assert_eq!(e.task(2).0, "succeeded");
    let gc = String::from_utf8_lossy(&e.forge("ok.sh", &["gc"]).stdout).to_string();
    assert!(
        gc.lines()
            .any(|l| l.starts_with("task 1 ") && l.contains("removed")),
        "{gc}"
    );
    assert!(!e.home.join("worktrees/1").exists());
}

/// A task queued while another runs is claimed on the next poll, not only
/// when the running one finishes: with a slot free, the worker's loop
/// wakes on its poll interval as well as on a join. Before 2026-09-19 it
/// woke only on a join or a signal, and two tasks sat queued for an hour
/// beside one running attempt and two free slots.
#[test]
fn a_task_queued_while_another_runs_is_claimed_before_it_finishes() {
    let e = Env::new();
    let first = e.add(&[]);
    let mut worker = Worker::spawn(
        e.cmd("ok.sh")
            .env("FAKE_SLEEP", "1")
            .env("FAKE_SLEEP_SECS", "15")
            .args(["work", "--jobs", "2", "--poll", "1"]),
    );
    assert!(
        wait_until(
            || e.attempts(first).first().is_some_and(|a| a.1 == "running"),
            Duration::from_secs(20)
        ),
        "the first task was never claimed"
    );
    let second = e.add(&[]);
    assert!(
        wait_until(
            || e.attempts(second).first().is_some_and(|a| a.1 == "running"),
            Duration::from_secs(10)
        ),
        "the second task was not claimed while the first ran: {:?}",
        e.task(second)
    );
    assert_eq!(
        e.attempts(first)[0].1,
        "running",
        "the first task was still running when the second was claimed"
    );
    assert!(worker.stop().success());
    assert_eq!(e.task(first).0, "succeeded");
    assert_eq!(e.task(second).0, "succeeded");
}
