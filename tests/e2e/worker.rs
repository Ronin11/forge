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
    // cache under FORGE_HOME/cache/repomap holds at least one blob.
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
        eprintln!("FORGE_TEST_NO_SANDBOX=1: skipping, bwrap unavailable");
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

/// The operator's claude config directory (`CLAUDE_CONFIG_DIR`) is seeded
/// into the sandbox with only what the CLI needs — the settings file — and
/// nothing else it holds is visible at that path (src/sandbox.rs, `command`).
#[test]
fn the_operators_config_directory_is_seeded_not_bound_into_the_sandbox() {
    let e = Env::new();
    if e.sandbox_disabled() {
        eprintln!("FORGE_TEST_NO_SANDBOX=1: skipping, bwrap unavailable");
        return;
    }
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    // Stands in for the operator's real claude config directory: one file
    // an attempt needs, one it must never see.
    let fake_config = e.home.join("fake-claude-config");
    std::fs::create_dir_all(&fake_config).unwrap();
    std::fs::write(fake_config.join("settings.json"), "operator-settings").unwrap();
    std::fs::write(
        fake_config.join("real-secret.txt"),
        "never-leaves-the-operator",
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/actions/canary.toml"),
        "name = \"canary\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"branch\"]\nrun = [\"bash\", \"-c\", \"set -e; test \\\"$(cat \\\"$CLAUDE_CONFIG_DIR/settings.json\\\")\\\" = operator-settings; test ! -e \\\"$CLAUDE_CONFIG_DIR/real-secret.txt\\\"\"]\n",
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/canary-wf.toml"),
        "name = \"canary-wf\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { action = \"code\" }, { action = \"canary\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let mut cmd = e.cmd("ok.sh");
    cmd.env("CLAUDE_CONFIG_DIR", &fake_config);
    let o = cmd
        .args([
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--workflow",
            "canary-wf",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = e.trace_json("1");
    let op = doc["ops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "canary")
        .unwrap_or_else(|| panic!("no canary op in {doc}"));
    assert_eq!(op["ok"], true, "{}", op["detail"]);
}

/// The private provider state (src/sandbox.rs, `provider_state_dir`) lives
/// as long as the task's worktree and never reaches another task's: a
/// retry in the same worktree sees what the first attempt wrote there (a
/// resumed session depends on that), a second task does not.
#[test]
fn provider_state_lives_for_the_task_and_never_reaches_another() {
    let e = Env::new();
    if e.sandbox_disabled() {
        eprintln!("FORGE_TEST_NO_SANDBOX=1: skipping, bwrap unavailable");
        return;
    }
    let o = e.run("provider-canary.sh", &["--retries", "1"]);
    assert!(
        o.status.success(),
        "the retry must find the first attempt's canary: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(e.attempts(1).len(), 2);
    let o = e.run("provider-canary.sh", &["--retries", "0"]);
    assert!(
        !o.status.success(),
        "a second task must not see the first task's canary: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(e.task(2).0, "failed");
}

/// A repository's cache (`FORGE_CACHE_DIR`) is private to it: a write aimed
/// at another repository's cache directory under the same `FORGE_HOME`
/// fails, since only this repository's own is bound in (src/sandbox.rs,
/// `set_cache_dir`/`cache_dir_for`; src/ctx.rs, `Forge::cache_dir`).
#[test]
fn a_write_to_another_repositorys_cache_directory_fails() {
    let e = Env::new();
    if e.sandbox_disabled() {
        eprintln!("FORGE_TEST_NO_SANDBOX=1: skipping, bwrap unavailable");
        return;
    }
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let foreign_cache = e.home.join("cache").join("some-other-repository");
    std::fs::create_dir_all(&foreign_cache).unwrap();
    let foreign_file = foreign_cache.join("poison");
    std::fs::write(
        e.home.join("workflows/actions/poison.toml"),
        format!(
            "name = \"poison\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"branch\"]\nrun = [\"bash\", \"-c\", {}]\n",
            serde_json::to_string(&format!(
                "! echo poison > '{}' 2>/dev/null",
                foreign_file.display()
            ))
            .unwrap()
        ),
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/poison-wf.toml"),
        "name = \"poison-wf\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { action = \"code\" }, { action = \"poison\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--workflow",
            "poison-wf",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        !foreign_file.exists(),
        "another repository's cache must never be written to from inside the sandbox"
    );
}

#[test]
fn doctor_warns_when_the_repomap_cache_is_missing() {
    let e = Env::new();
    // Bootstraps FORGE_HOME without ever running a task, so
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
fn doctor_warns_about_a_held_initiative_and_names_it() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo],
        )
        .status
        .success()
    );

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("tasks.txt");
    // Two independent tasks: the first lands and spends the whole
    // budget, the second is left queued behind the hold.
    std::fs::write(&file, "first task\n\nsecond task").unwrap();

    let o = e.forge(
        "ok.sh",
        &[
            "initiative",
            "new",
            "demo",
            "--outcome",
            "both tasks land within budget",
            "--from",
            file.to_str().unwrap(),
            // `ok.sh` reports total_cost_usd 0.01 per attempt, so the
            // initiative's cost reaches this budget the moment the first
            // task finishes.
            "--budget",
            "0.01",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("created initiative "))
        .unwrap()
        .parse()
        .unwrap();

    assert!(
        e.forge("ok.sh", &["work", "--once", "--max-tasks", "1"])
            .status
            .success()
    );
    assert_eq!(e.task(1).0, "succeeded");
    assert_eq!(e.task(2).0, "queued");

    // Doctor is the only place besides `forge initiative show <id>` that
    // says this initiative is held: nothing else about an idle worker
    // with an empty-looking queue would say so.
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("WARN initiatives"), "{out}");
    assert!(out.contains(&format!("initiative {id}")), "{out}");
    assert!(out.contains("budget: $0.01 of $0.01"), "{out}");
    assert!(out.contains("1 task(s) queued behind the hold"), "{out}");
    assert!(out.contains(&format!("forge initiative set {id}")), "{out}");

    let o = e.forge("ok.sh", &["doctor", "--json"]);
    let checks: Vec<serde_json::Value> = serde_json::from_slice(&o.stdout).unwrap();
    assert!(
        checks
            .iter()
            .any(|c| c["name"] == "initiatives" && c["status"] == "warn"),
        "missing initiatives row in {checks:?}"
    );

    // Raising the budget lifts the hold; doctor goes back to OK.
    assert!(
        e.forge(
            "ok.sh",
            &["initiative", "set", &id.to_string(), "--budget", "5"],
        )
        .status
        .success()
    );
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("OK   initiatives"), "{out}");
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

/// `--older-than` pushes and removes a retained worktree whose task
/// finished long enough ago even though it was never published, so
/// nothing sits on disk forever just because it was never merged; a
/// young unpublished worktree is left exactly as plain `forge gc` would
/// leave it.
#[test]
fn gc_older_than_pushes_and_removes_an_old_unpublished_worktree_but_keeps_a_young_one() {
    let e = Env::new();
    assert!(!e.run("wrong.sh", &["--retries", "0"]).status.success());
    assert!(!e.run("wrong.sh", &["--retries", "0"]).status.success());

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    e.db()
        .execute(
            "UPDATE tasks SET finished_at=?1 WHERE id=1",
            [now - 30 * 86_400],
        )
        .unwrap();
    assert!(
        !e.origin_branches().contains("forge/1-"),
        "task 1's branch starts out unpublished"
    );

    let o = e.forge("ok.sh", &["gc", "--older-than", "7"]);
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
        "the old task's branch was pushed before its worktree was removed"
    );
    assert!(
        !e.origin_branches().contains("forge/2-"),
        "the young task's unpublished branch is left alone"
    );
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
            .env("FAKE_SLEEP_SECS", "30")
            .args(["work", "--jobs", "2", "--poll", "1"]),
    );
    assert!(
        wait_until(
            || e.attempts(first).first().is_some_and(|a| a.1 == "running"),
            Duration::from_secs(30)
        ),
        "the first task was never claimed"
    );
    let second = e.add(&[]);
    assert!(
        wait_until(
            || e.attempts(second).first().is_some_and(|a| a.1 == "running"),
            Duration::from_secs(15)
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

/// Whether this machine can give a sandbox a network namespace of its own;
/// nested inside another bwrap it cannot, and the egress tests skip.
fn can_unshare_net() -> bool {
    std::process::Command::new("bwrap")
        .args(["--unshare-net", "--ro-bind", "/", "/", "true"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A workflow that runs the built-in `egress-probe` after the agent, and
/// the ops it left behind for task `id`.
fn probe_workflow(e: &Env) {
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/probed.toml"),
        "name = \"probed\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { action = \"code\" }, { action = \"egress-probe\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
}

fn probe_detail(e: &Env, id: i64) -> (bool, String) {
    let doc = e.trace_json(id);
    let probe = doc["ops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "egress-probe")
        .unwrap_or_else(|| panic!("no egress-probe op in {doc}"))
        .clone();
    (
        probe["ok"].as_bool().unwrap(),
        format!(
            "{}{}",
            probe["output"].as_str().unwrap_or(""),
            probe["detail"].as_str().unwrap_or("")
        ),
    )
}

#[test]
fn the_egress_probe_passes_in_the_sandbox_and_shows_the_declared_policy() {
    let e = Env::new();
    if e.sandbox_disabled() || !can_unshare_net() {
        eprintln!("no bwrap network namespace here: skipping");
        return;
    }
    probe_workflow(&e);
    // The repository declares one registry; the model endpoint is implied.
    let toml = std::fs::read_to_string(e.repo.join("forge.toml")).unwrap();
    std::fs::write(
        e.repo.join("forge.toml"),
        format!("{toml}[sandbox]\negress = [\"registry.npmjs.org\"]\n"),
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "declare egress"]);
    let o = e.run("ok.sh", &["--workflow", "probed", "--retries", "0"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (ok, detail) = probe_detail(&e, 1);
    assert!(ok, "{detail}");
    assert!(
        detail.contains("direct  1.1.1.1:443 is unreachable"),
        "{detail}"
    );
    assert!(detail.contains("allow registry.npmjs.org"), "{detail}");
    assert!(
        detail.contains("allow *.anthropic.com"),
        "the model endpoint is always allowed: {detail}"
    );
    assert!(
        detail.contains("CONNECT to a host off the list: HTTP/1.1 403"),
        "{detail}"
    );
}

#[test]
fn an_attempt_cannot_open_a_connection_out_and_only_the_proxy_answers() {
    let e = Env::new();
    if e.sandbox_disabled() || !can_unshare_net() {
        eprintln!("no bwrap network namespace here: skipping");
        return;
    }
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    // A check that must reach only loopback: the local server it starts is
    // reachable (NO_PROXY), a public address is not.
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\nlocal = [\"bash\", \"-c\", \"exec 3<>/dev/tcp/127.0.0.1/3128\"]\nout = [\"bash\", \"-c\", \"! timeout 5 bash -c 'exec 3<>/dev/tcp/1.1.1.1/443' 2>/dev/null\"]\n",
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "network checks"]);
    let o = e.run("ok.sh", &["--retries", "0"]);
    assert!(
        o.status.success(),
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn the_egress_probe_fails_when_there_is_no_sandbox() {
    let e = Env::new();
    probe_workflow(&e);
    let mut cmd = e.cmd("ok.sh");
    cmd.env("FORGE_SANDBOX", "0")
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy");
    let o = cmd
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--no-land",
            "--workflow",
            "probed",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    assert!(
        !o.status.success(),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    let (ok, detail) = probe_detail(&e, 1);
    assert!(!ok, "{detail}");
    assert!(detail.contains("not in the egress sandbox"), "{detail}");
}

#[test]
fn doctor_reports_each_projects_egress_policy_and_warns_when_the_sandbox_is_off() {
    let e = Env::new();
    let toml = std::fs::read_to_string(e.repo.join("forge.toml")).unwrap();
    std::fs::write(
        e.repo.join("forge.toml"),
        format!("{toml}[sandbox]\negress = [\"registry.npmjs.org\", \"*.crates.io\"]\n"),
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "declare egress"]);
    // `add` creates the project that owns the repository.
    e.add(&[]);

    let mut off = e.cmd("ok.sh");
    off.env("FORGE_SANDBOX", "0").arg("doctor");
    let o = off.output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("WARN egress "), "{out}");
    assert!(out.contains("no egress policy is enforced"), "{out}");
    assert!(
        out.contains("WARN egress.repo") && out.contains("*.crates.io, registry.npmjs.org"),
        "{out}"
    );
    assert!(out.contains("the model endpoint and"), "{out}");

    if e.sandbox_disabled() || !can_unshare_net() {
        return;
    }
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("OK   egress "), "{out}");
    assert!(
        out.contains("*.anthropic.com"),
        "the model endpoint is named: {out}"
    );
    assert!(out.contains("OK   egress.repo"), "{out}");
}
