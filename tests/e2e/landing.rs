use crate::support::*;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

fn origin_sha(e: &Env, branch: &str) -> String {
    let o = Command::new("git")
        .args([
            "--git-dir",
            e.origin.to_str().unwrap(),
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn a_verified_task_lands_on_the_base_and_the_next_task_starts_from_it() {
    let e = Env::new();
    assert_eq!(origin_sha(&e, "main"), "", "the remote has no main yet");
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(reason.starts_with("landed main @ "), "{reason}");
    assert!(pushed);
    let main = origin_sha(&e, "main");
    assert_eq!(
        main,
        origin_sha(&e, "forge/1-write-42"),
        "main fast-forwarded to the branch"
    );
    assert_eq!(
        origin_file(&e, "main", "answer.txt").as_deref(),
        Some("42\n")
    );
    assert_eq!(
        op_names(&e, 1),
        vec![
            ("clone".into(), true),
            ("setup".into(), true),
            ("repo-map".into(), true),
            ("verify".into(), true),
            ("integrate".into(), true),
            ("push".into(), true),
            ("land".into(), true)
        ]
    );
    // The registered checkout's own main is untouched: it is the operator's.
    assert_ne!(git(&e.repo, &["rev-parse", "main"]), main);
    // The next task starts from the remote's main, which has the answer.
    let o = e.forge(
        "addfile.sh",
        &[
            "run",
            e.repo.to_str().unwrap(),
            "add extra",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let base: String = e
        .db()
        .query_row("SELECT base_sha FROM tasks WHERE id=2", [], |r| r.get(0))
        .unwrap();
    assert_eq!(base, main, "task 2 started from what task 1 landed");
    assert_eq!(
        origin_file(&e, "main", "extra.txt").as_deref(),
        Some("extra\n")
    );
    assert_eq!(
        origin_file(&e, "main", "answer.txt").as_deref(),
        Some("42\n")
    );
    let o = e.forge("ok.sh", &["show", "1"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("landed main @"));
    // The landing is a column the scheduler reads, not a wording of the reason.
    let landed: String = e
        .db()
        .query_row("SELECT landed_sha FROM tasks WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(landed.len(), 40, "{landed}");
    let (_, reason, _) = e.task(1);
    assert!(reason.contains(&landed[..8]), "{reason} vs {landed}");
}

#[test]
fn no_land_leaves_the_verified_branch_for_a_human() {
    let e = Env::new();
    assert!(e.run("ok.sh", &["--retries", "0"]).status.success());
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(!reason.starts_with("landed"), "{reason}");
    assert!(pushed);
    assert_eq!(origin_sha(&e, "main"), "", "main was not created");
    assert!(
        op_names(&e, 1)
            .iter()
            .all(|(n, _)| n != "integrate" && n != "land")
    );
    let o = e.forge("ok.sh", &["show", "1"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("land       manual"));
}

#[test]
fn forge_land_lands_a_verified_no_land_task_by_hand() {
    let e = Env::new();
    assert!(e.run("ok.sh", &["--retries", "0"]).status.success());
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(!reason.starts_with("landed"), "{reason}");
    assert!(pushed);
    assert_eq!(origin_sha(&e, "main"), "", "main was not created yet");

    let o = e.forge("ok.sh", &["land", "1"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("landed task 1 on main @ "), "{out}");

    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(reason.starts_with("landed main @ "), "{reason}");
    assert!(pushed);
    let main = origin_sha(&e, "main");
    assert_ne!(main, "");
    assert_eq!(
        main,
        origin_sha(&e, "forge/1-write-42-to-answertxt"),
        "main fast-forwarded to the branch"
    );
    assert_eq!(
        origin_file(&e, "main", "answer.txt").as_deref(),
        Some("42\n")
    );
    let o = e.forge("ok.sh", &["show", "1"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("landed main @"));

    // Landing an already-landed task is refused.
    let o = e.forge("ok.sh", &["land", "1"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("already landed"));
}

#[test]
fn a_conflicting_landing_goes_back_to_the_coder_who_merges_the_base() {
    let e = Env::new();
    // Any non-empty answer will do: the two tasks disagree on it.
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\nanswer = [\"bash\", \"-c\", \"test -s answer.txt\"]\nshell = [\"bash\", \"-n\", \"hello.sh\"]\n",
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "any answer"]);
    let a = e.add(&["--retries", "1"]);
    let o = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 43 to answer.txt",
            "--retries",
            "1",
        ],
    );
    assert!(o.status.success());
    let o = e.forge("echoanswer.sh", &["work", "--once", "--jobs", "2"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    for id in [a, 2] {
        let (state, reason, _) = e.task(id);
        assert_eq!(state, "succeeded", "task {id}: {reason}");
        assert!(reason.starts_with("landed main @ "), "{reason}");
    }
    // One of them found main moved, conflicted, and its coder merged.
    let conflicted: Vec<i64> = [a, 2]
        .into_iter()
        .filter(|&id| {
            op_names(&e, id)
                .iter()
                .any(|(n, ok)| n == "integrate" && !ok)
        })
        .collect();
    assert_eq!(conflicted.len(), 1, "{err}");
    let id = conflicted[0];
    assert_eq!(e.attempts(id).len(), 2, "the coder ran once more to merge");
    assert!(
        e.log_text(id, 2).contains("git merge forge/main"),
        "the coder was told how"
    );
    let ops = op_names(&e, id);
    let names: Vec<&str> = ops.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "clone",
            "setup",
            "repo-map",
            "verify",
            "integrate",
            "verify",
            "integrate",
            "push",
            "land"
        ],
        "{ops:?}"
    );
    // main holds both landings, the second as a merge.
    let merges = Command::new("git")
        .args([
            "--git-dir",
            e.origin.to_str().unwrap(),
            "rev-list",
            "--merges",
            "--count",
            "main",
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&merges.stdout).trim(), "1");
    let answer = origin_file(&e, "main", "answer.txt").unwrap();
    assert!(answer == "42\n" || answer == "43\n", "{answer}");
}

#[test]
fn a_conflict_the_budget_cannot_cover_fails_the_task_and_pushes_the_verified_branch() {
    let e = Env::new();
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\nanswer = [\"bash\", \"-c\", \"test -s answer.txt\"]\nshell = [\"bash\", \"-n\", \"hello.sh\"]\n",
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "any answer"]);
    // Each attempt costs $0.01; a $0.01 budget covers the first and not the merge.
    for task in ["write 42 to answer.txt", "write 43 to answer.txt"] {
        let o = e.forge(
            "ok.sh",
            &[
                "add",
                e.repo.to_str().unwrap(),
                task,
                "--retries",
                "1",
                "--budget",
                "0.01",
            ],
        );
        assert!(o.status.success());
    }
    let o = e.forge("echoanswer.sh", &["work", "--once", "--jobs", "2"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let states: Vec<(String, String, bool)> = [1, 2].into_iter().map(|id| e.task(id)).collect();
    let landed = states
        .iter()
        .filter(|(s, r, _)| s == "succeeded" && r.starts_with("landed"))
        .count();
    assert_eq!(landed, 1, "{states:?}");
    let stalled: Vec<&(String, String, bool)> =
        states.iter().filter(|(s, _, _)| s == "failed").collect();
    assert_eq!(stalled.len(), 1, "{states:?}");
    let (_, reason, pushed) = stalled[0];
    assert!(
        reason.starts_with("landing failed: main moved to"),
        "{reason}"
    );
    assert!(reason.contains("the task budget is spent"), "{reason}");
    assert!(pushed, "the verified branch is not lost");
    let id = if states[0].0 == "failed" { 1 } else { 2 };
    let o = e.forge("ok.sh", &["show", &id.to_string()]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("The branch is pushed"));
}

#[test]
fn a_task_is_judged_by_the_hidden_suite_that_matches_its_base_not_one_that_grew_meanwhile() {
    let e = Env::new();
    // Acceptance scripts run when present; none is fine.
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\nshell = [\"bash\", \"-n\", \"hello.sh\"]\ntest = [\"bash\", \"-c\", \"shopt -s nullglob; for f in tests/acceptance/*.sh; do bash \\\"$f\\\" || exit 1; done\"]\n[verify]\nnamespace = [\"tests/acceptance/\"]\n",
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "acceptance layout"]);
    // B starts first and takes a while; it does not write the answer.
    let child = e
        .cmd("addfile.sh")
        .env("FAKE_SLEEP", "1")
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "add extra",
            "--retries",
            "0",
        ])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    assert!(wait_until(
        || {
            e.home.join("forge.db").exists()
                && e.db()
                    .query_row(
                        "SELECT 1 FROM tasks WHERE id=1 AND base_sha != ''",
                        [],
                        |r| r.get::<_, i64>(0),
                    )
                    .is_ok()
        },
        Duration::from_secs(10)
    ));
    // A, a tdd task, lands meanwhile and folds "answer.txt must be 42" into forge-verify.
    let mut c = e.with_role("ok.sh", "TESTS", "testwriter.sh");
    let a = c
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--workflow",
            "tdd",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    assert!(a.status.success(), "{}", String::from_utf8_lossy(&a.stderr));
    assert!(origin_file(&e, "forge-verify", "tests/acceptance/answer.sh").is_some());
    // B is judged by the suite as of its base (none), then lands against the current one.
    let o = child.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(reason.starts_with("landed main @ "), "{reason}");
    assert!(
        !err.contains("verification file(s) from forge-verify\n  ✗"),
        "{err}"
    );
    assert!(
        err.contains("overlay  1 verification file(s) from forge-verify\n")
            || err.contains("from forge-verify"),
        "the landing used the current suite: {err}"
    );
    assert_eq!(
        origin_file(&e, "main", "extra.txt").as_deref(),
        Some("extra\n")
    );
    assert_eq!(
        origin_file(&e, "main", "answer.txt").as_deref(),
        Some("42\n")
    );
    let doc: serde_json::Value = e.trace_json("1");
    assert_eq!(
        doc["task"]["verify_base"], "",
        "no standing suite existed when B started"
    );
    assert!(
        doc["attempts"][0]["inputs"]["overlay_refs"]
            .as_array()
            .unwrap()
            .is_empty(),
        "the coder's verify overlaid nothing"
    );
}

#[test]
fn landing_reverifies_against_the_moved_base_and_folds_the_hidden_tests() {
    let e = Env::new();
    tdd_repo(&e);
    // The coder is slow enough for main to move underneath it.
    let mut c = e.with_role("feedbackcoder.sh", "TESTS", "testwriter.sh");
    c.env("FAKE_SLEEP", "1");
    let child = c
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--workflow",
            "tdd",
            "--retries",
            "1",
        ])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Once the task has cloned its base, main gains a check the branch does not satisfy.
    assert!(
        wait_until(
            || {
                e.home
                    .join("forge.db")
                    .exists()
                    .then(|| {
                        e.db()
                            .query_row(
                                "SELECT base_sha FROM tasks WHERE id=1 AND base_sha != ''",
                                [],
                                |r| r.get::<_, String>(0),
                            )
                            .ok()
                    })
                    .flatten()
                    .is_some()
            },
            Duration::from_secs(10)
        ),
        "the task never cloned"
    );
    let other = e.repo.parent().unwrap().join("other");
    let o = Command::new("git")
        .args([
            "clone",
            "-q",
            e.repo.to_str().unwrap(),
            other.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(o.status.success());
    let toml = std::fs::read_to_string(other.join("forge.toml")).unwrap();
    std::fs::write(
        other.join("forge.toml"),
        toml.replace(
            "[checks]\n",
            "[checks]\nextra = [\"bash\", \"-c\", \"test -f extra.txt\"]\n",
        ),
    )
    .unwrap();
    git(&other, &["config", "user.name", "Other"]);
    git(&other, &["config", "user.email", "other@example.com"]);
    git(&other, &["commit", "-qam", "main now wants extra.txt"]);
    git(
        &other,
        &["push", "-q", e.origin.to_str().unwrap(), "main:main"],
    );
    let o = child.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(reason.starts_with("landed main @ "), "{reason}");
    assert!(err.contains("integrate merged main @"), "{err}");
    assert!(
        err.contains("L1 failed: extra"),
        "the merged tree was verified against main's checks: {err}"
    );
    assert!(
        err.contains("land     back to code for another attempt"),
        "{err}"
    );
    assert!(
        e.log_text(1, 4).contains("verification fails"),
        "the coder saw why"
    );
    let names: Vec<String> = op_names(&e, 1).into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        names,
        vec![
            "clone",
            "verify",
            "setup",
            "repo-map",
            "verify",
            "integrate",
            "verify",
            "integrate",
            "push",
            "land"
        ],
        "{names:?}"
    );
    // The integrator's own check run leaves a record, not just a line in
    // the coder's feedback: an attempts row, step "integrate", no agent,
    // with the failing check named.
    let integrate_attempts: Vec<(String, String)> = {
        let c = e.db();
        let mut s = c
            .prepare(
                "SELECT state, verdict_json FROM attempts WHERE task_id=1 AND step='integrate' ORDER BY attempt_no",
            )
            .unwrap();
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(integrate_attempts.len(), 1, "{integrate_attempts:?}");
    let (state, verdict_json) = &integrate_attempts[0];
    assert_eq!(state, "checks_failed");
    assert_eq!(check(verdict_json, "L1", "extra"), Some(false));
    // forge trace --json and forge show both carry it, no agent involved.
    let doc = e.trace_json(1);
    let integrate = doc["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["step"] == "integrate")
        .expect("integrate attempt in trace");
    assert_eq!(integrate["num_turns"], 0);
    assert_eq!(integrate["cost_usd"], serde_json::Value::Null);
    assert_eq!(integrate["inputs"]["model"], "");
    let o = e.forge("ok.sh", &["show", "1"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("[integrate]"), "{out}");
    assert!(out.contains("✗ L1 extra"), "{out}");
    assert_eq!(
        origin_file(&e, "main", "extra.txt").as_deref(),
        Some("extra\n")
    );
    assert_eq!(
        origin_file(&e, "main", "answer.txt").as_deref(),
        Some("42\n")
    );
    // The task's hidden test joined the standing suite, locally and on the remote.
    assert!(
        git(
            &e.repo,
            &[
                "ls-tree",
                "--name-only",
                "forge-verify",
                "tests/acceptance/"
            ]
        )
        .contains("tests/acceptance/answer.sh")
    );
    assert!(origin_file(&e, "forge-verify", "tests/acceptance/answer.sh").is_some());
    assert!(
        err.contains("1 hidden test file(s) folded into forge-verify"),
        "{err}"
    );
}

#[test]
fn a_failed_push_ends_the_task_failed_not_succeeded() {
    // Verified work that could not be published is not a success.
    let e = Env::new();
    let lock = |mode: &str| {
        assert!(
            Command::new("chmod")
                .args(["-R", mode])
                .arg(&e.origin)
                .status()
                .unwrap()
                .success()
        );
    };
    lock("a-w");
    let o = e.run("ok.sh", &[]);
    lock("u+w");
    assert!(
        !o.status.success(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "failed", "{reason}");
    assert!(reason.starts_with("push failed: "), "{reason}");
    assert!(!pushed);
    let a = e.attempts(1);
    assert_eq!(
        a[0].1, "succeeded",
        "the attempt itself was verified: {a:?}"
    );
}

#[test]
fn a_task_queued_after_another_waits_for_its_landing_and_blocks_on_its_failure() {
    let e = Env::new();
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\nanswer = [\"bash\", \"-c\", \"test -s answer.txt\"]\nshell = [\"bash\", \"-n\", \"hello.sh\"]\n",
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "any answer"]);
    // 1 lands; 2 waits on 1 and must start from what 1 landed.
    let a = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--retries",
            "0",
        ],
    );
    assert!(a.status.success());
    let b = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 43 to answer.txt",
            "--retries",
            "0",
            "--after",
            "1",
        ],
    );
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let bad = e.forge(
        "ok.sh",
        &["add", e.repo.to_str().unwrap(), "write 44", "--after", "99"],
    );
    assert!(!bad.status.success(), "an unknown dependency is refused");
    let o = e.forge("echoanswer.sh", &["work", "--once", "--jobs", "2"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(1).0, "succeeded");
    assert_eq!(e.task(2).0, "succeeded");
    let landed_by_1: String = e
        .db()
        .query_row("SELECT base_sha FROM tasks WHERE id=2", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        landed_by_1,
        origin_sha(&e, "forge/1-write-42-to-answertxt"),
        "task 2 started from task 1's landing"
    );
    assert_eq!(
        origin_file(&e, "main", "answer.txt").as_deref(),
        Some("43\n")
    );
    let o = e.forge("ok.sh", &["show", "2"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("after      1"));

    // 3 fails its check; 4 waits on 3 and is blocked with the reason, never run.
    let c = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write nothing useful",
            "--retries",
            "0",
            "--check",
            "false",
        ],
    );
    assert!(c.status.success());
    let d = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 45 to answer.txt",
            "--retries",
            "0",
            "--after",
            "3",
        ],
    );
    assert!(d.status.success());
    let o = e.forge("echoanswer.sh", &["work", "--once"]);
    assert!(o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(e.task(3).0, "failed");
    let (state, reason, _) = e.task(4);
    assert_eq!(state, "blocked", "{reason}");
    assert!(reason.starts_with("waits on task 3 (failed: "), "{reason}");
    assert!(err.contains("task 4 blocked: waits on task 3"), "{err}");
    assert_eq!(e.attempts(4).len(), 0, "never ran");
    // A task that never ran says nothing about its workflow.
    let stats = String::from_utf8_lossy(&e.forge("ok.sh", &["stats"]).stdout).to_string();
    let direct = stats
        .lines()
        .find(|l| {
            l.starts_with("direct ") && l.split_whitespace().nth(1).is_some_and(|h| h.len() > 8)
        })
        .unwrap_or("");
    assert_eq!(
        direct.split_whitespace().nth(2),
        Some("3"),
        "tasks 1-3 ran, 4 never did: {stats}"
    );
    let o = e.forge("ok.sh", &["show", "4"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("forge retry"));

    let reqs: serde_json::Value = e.requests_json();
    assert_eq!(reqs.as_array().unwrap()[0]["kind"], "dependency");
    // retry: 4 alone is refused (3 never landed); 3 --chain re-queues 3 and 4 with 4 waiting on the new 3.
    let o = e.forge("ok.sh", &["retry", "4"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("dependency 3 ended without landing"));
    // retry 3: 4, which was blocked by 3's failure, is queued again behind the new task.
    let o = e.forge(
        "ok.sh",
        &[
            "retry",
            "3",
            "--chain",
            "--retries",
            "1",
            "--max-turns",
            "77",
            "--timeout-secs",
            "99",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("retried task 3 as 5"), "{out}");
    // The chain re-points 4's after list at the new task, but no longer
    // releases 4 itself: that takes 5 actually reaching a terminal state
    // (see `a_task_blocked_on_a_failed_dependency_is_released_once_its_after_list_points_at_a_task_that_lands`).
    let doc: serde_json::Value = e.trace_json("4");
    assert_eq!(doc["task"]["state"], "blocked", "{doc}");
    assert_eq!(doc["task"]["after"], serde_json::json!([5]));
    assert!(doc["task"]["retry_of"].is_null());
    // The overrides reach the retried task, not the chained dependent.
    let (turns, timeout): (i64, i64) = e
        .db()
        .query_row(
            "SELECT max_turns, timeout_secs FROM tasks WHERE id=5",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((turns, timeout), (77, 99));
    let turns4: i64 = e
        .db()
        .query_row("SELECT max_turns FROM tasks WHERE id=4", [], |r| r.get(0))
        .unwrap();
    assert_ne!(turns4, 77, "the dependent keeps its own turns");
    let five: serde_json::Value = e.trace_json("5");
    assert_eq!(
        five["task"]["max_attempts"], 2,
        "the override applies to the retried task"
    );
    // Parent, children, root, and the whole chain, from either end.
    let three: serde_json::Value = e.trace_json("3");
    assert_eq!(three["task"]["children"], serde_json::json!([5]));
    assert_eq!(three["task"]["root"], 3);
    assert_eq!(five["task"]["parent"], 3);
    assert_eq!(five["task"]["root"], 3);
    let chain: Vec<i64> = five["task"]["lineage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_i64().unwrap())
        .collect();
    assert_eq!(chain, vec![3, 5]);
    let o = e.forge("ok.sh", &["show", "5"]);
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("lineage    3 failed → [5 queued]"),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    // Shown from the root, the lineage line still appears: root first, current task bracketed.
    let o = e.forge("ok.sh", &["show", "3"]);
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("lineage    [3 failed] → 5 queued"),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    // Machine-readable listings for a client.
    let log: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["log", "--json"]).stdout).unwrap();
    assert_eq!(log.as_array().unwrap().len(), 5);
    // A rerouted dependent still names its now-stale, failed dependency
    // until the new task it was re-pointed at actually lands: it has not
    // left the human queue yet.
    let reqs: serde_json::Value = e.requests_json();
    assert_eq!(reqs.as_array().unwrap().len(), 1, "{reqs}");
    assert_eq!(reqs.as_array().unwrap()[0]["id"], 4);
}

#[test]
fn a_task_on_another_repository_waits_on_a_dependency_and_runs_after_it_lands() {
    let e = Env::new();
    let repo2 = e._dir.path().join("repo2");
    let origin2 = e._dir.path().join("origin2.git");
    std::fs::create_dir_all(&repo2).unwrap();
    git(&repo2, &["init", "-q", "-b", "main"]);
    git(&repo2, &["config", "user.name", "Test"]);
    git(&repo2, &["config", "user.email", "test@example.com"]);
    std::fs::write(
        repo2.join("forge.toml"),
        "[checks]\nanswer = [\"bash\", \"-c\", \"test -s answer.txt\"]\n",
    )
    .unwrap();
    git(&repo2, &["add", "-A"]);
    git(&repo2, &["commit", "-qm", "init"]);
    Command::new("git")
        .args(["init", "-q", "--bare"])
        .arg(&origin2)
        .status()
        .unwrap();
    git(
        &repo2,
        &["remote", "add", "origin", origin2.to_str().unwrap()],
    );
    let origin2_file = |branch: &str, path: &str| -> Option<String> {
        let o = Command::new("git")
            .args([
                "--git-dir",
                origin2.to_str().unwrap(),
                "show",
                &format!("{branch}:{path}"),
            ])
            .output()
            .unwrap();
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).to_string())
    };

    // A task on repo2 --after a task on repo1: once refused outright
    // ("that task is in ..., not this repository"), now accepted, since a
    // dependency only means waiting on the other task's terminal state.
    let a = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--retries",
            "0",
        ],
    );
    assert!(a.status.success());
    let b = e.forge(
        "ok.sh",
        &[
            "add",
            repo2.to_str().unwrap(),
            "write 43 to answer.txt",
            "--retries",
            "0",
            "--after",
            "1",
        ],
    );
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));

    let o = e.forge("ok.sh", &["work", "--once", "--jobs", "2"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(1).0, "succeeded");
    assert_eq!(e.task(2).0, "succeeded");
    assert_eq!(
        origin_file(&e, "main", "answer.txt").as_deref(),
        Some("42\n"),
        "task 1 landed on repo1's own base"
    );
    assert_eq!(
        origin2_file("main", "answer.txt").as_deref(),
        Some("42\n"),
        "task 2 landed on repo2's own base"
    );

    let doc: serde_json::Value = e.trace_json("2");
    assert_eq!(doc["task"]["after"], serde_json::json!([1]));

    // A dependency across repositories still blocks a dependent on failure.
    let c = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write nothing useful",
            "--retries",
            "0",
            "--check",
            "false",
        ],
    );
    assert!(c.status.success());
    let d = e.forge(
        "ok.sh",
        &[
            "add",
            repo2.to_str().unwrap(),
            "write 44 to answer.txt",
            "--retries",
            "0",
            "--after",
            "3",
        ],
    );
    assert!(d.status.success());
    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(o.status.success());
    assert_eq!(e.task(3).0, "failed");
    let (state, reason, _) = e.task(4);
    assert_eq!(state, "blocked", "{reason}");
    assert!(reason.starts_with("waits on task 3 (failed: "), "{reason}");
    assert_eq!(e.attempts(4).len(), 0, "never ran");
}

#[test]
fn integrate_merges_verified_branches_in_order_and_reverifies_or_stops_at_the_conflict() {
    let e = Env::new();
    // Only the shell check: each task adds its own file, and the third contradicts the first.
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\nshell = [\"bash\", \"-n\", \"hello.sh\"]\n",
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "shell only"]);
    assert!(e.run("ok.sh", &["--retries", "0"]).status.success());
    assert!(
        e.forge(
            "addfile.sh",
            &[
                "run",
                e.repo.to_str().unwrap(),
                "add extra",
                "--no-land",
                "--retries",
                "0"
            ]
        )
        .status
        .success()
    );
    let o = e.forge("ok.sh", &["integrate", "1", "2"]);
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(
        o.status.success(),
        "{out}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        out.contains("task 1    merged") && out.contains("task 2    merged"),
        "{out}"
    );
    assert!(
        out.contains("task 2    verified with everything before it"),
        "{out}"
    );
    let branch = out
        .lines()
        .find(|l| l.starts_with("integrated"))
        .unwrap()
        .split_whitespace()
        .find(|w| w.starts_with("forge/integration-"))
        .unwrap()
        .to_string();
    assert_eq!(
        git(&e.repo, &["show", &format!("{branch}:answer.txt")]),
        "42"
    );
    assert_eq!(
        git(&e.repo, &["show", &format!("{branch}:extra.txt")]),
        "extra"
    );
    // A third branch that conflicts stops the integration and says where.
    assert!(
        e.forge(
            "echoanswer.sh",
            &[
                "run",
                e.repo.to_str().unwrap(),
                "write 43 to answer.txt",
                "--no-land",
                "--retries",
                "0"
            ]
        )
        .status
        .success()
    );
    let o = e.forge("ok.sh", &["integrate", "1", "3"]);
    assert!(!o.status.success());
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(out.contains("task 3    CONFLICT in answer.txt"), "{out}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("conflicts with what came before it"));
}

#[test]
fn a_blocked_dependency_keeps_its_dependents_waiting_and_a_retry_carries_them_along() {
    // 1 asks a question; 2 waits on 1. A blocked task is not finished, so 2
    // stays queued; answering 1 re-queues it as 3, and 2 now waits on 3.
    let e = Env::new();
    let a = e.forge(
        "needsinput.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--retries",
            "0",
        ],
    );
    assert!(a.status.success());
    let b = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt too",
            "--retries",
            "0",
            "--after",
            "1",
        ],
    );
    assert!(b.status.success());
    let o = e.forge("needsinput.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(1).0, "blocked");
    assert_eq!(
        e.task(2).0,
        "queued",
        "a blocked dependency does not sweep its dependents: {:?}",
        e.task(2)
    );
    let o = e.forge("ok.sh", &["answer", "1", "answer.txt, lowercase"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let after: String = e
        .db()
        .query_row("SELECT after_json FROM tasks WHERE id=2", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, "[3]", "the dependent follows the retry");
    // 3 lands the answer; then 2 runs from that landing and adds its own file.
    let o = e.forge("ok.sh", &["work", "--once", "--max-tasks", "1"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(3).0, "succeeded");
    let o = e.forge("addfile.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(2).0, "succeeded", "{:?}", e.task(2));
    // A dependent already swept into blocked by a failed dependency is
    // queued again when that dependency is retried.
    let c = e.forge(
        "wrong.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt again",
            "--retries",
            "0",
        ],
    );
    assert!(c.status.success());
    let d = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "and once more",
            "--retries",
            "0",
            "--after",
            "4",
        ],
    );
    assert!(d.status.success());
    let o = e.forge("wrong.sh", &["work", "--once"]);
    assert!(
        !o.status.success() || e.task(4).0 == "failed",
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(e.task(4).0, "failed");
    let o = e.forge("ok.sh", &["work", "--once"]);
    let _ = o;
    assert_eq!(e.task(5).0, "blocked", "{:?}", e.task(5));
    assert!(
        e.task(5).1.starts_with("waits on task 4 "),
        "{:?}",
        e.task(5)
    );
    assert!(e.forge("ok.sh", &["retry", "4"]).status.success());
    // The reroute re-points the after list, but release is no longer the
    // retry's own job: 5 stays blocked, with its stale reason, until 6
    // itself reaches a terminal state.
    assert_eq!(e.task(5).0, "blocked", "{:?}", e.task(5));
    assert!(
        e.task(5).1.starts_with("waits on task 4 "),
        "{:?}",
        e.task(5)
    );
    let after: String = e
        .db()
        .query_row("SELECT after_json FROM tasks WHERE id=5", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, "[6]");
    // What happens once 6 actually reaches a terminal state -- 5 released
    // to queued, or reblocked with a fresh reason -- is covered by
    // `a_task_blocked_on_a_failed_dependency_is_released_once_its_after_list_points_at_a_task_that_lands`.
}

#[test]
fn a_task_blocked_on_a_failed_dependency_is_released_once_its_after_list_points_at_a_task_that_lands()
 {
    // a and a3 each fail independently; b waits on a, c waits on a3, so a
    // retry of a (which only reroutes a's own dependents) never touches c.
    let e = Env::new();
    let a = e.add(&["--retries", "0"]);
    let b = e.add(&["--retries", "0", "--after", &a.to_string()]);
    let a3 = e.add(&["--retries", "0"]);
    let c = e.add(&["--retries", "0", "--after", &a3.to_string()]);
    let o = e.forge("wrong.sh", &["work", "--once"]);
    let _ = o;
    assert_eq!(e.task(a).0, "failed");
    assert_eq!(e.task(a3).0, "failed");
    let o = e.forge("ok.sh", &["work", "--once"]);
    let _ = o;
    assert_eq!(e.task(b).0, "blocked", "{:?}", e.task(b));
    assert!(
        e.task(b).1.starts_with(&format!("waits on task {a} ")),
        "{:?}",
        e.task(b)
    );
    assert_eq!(e.task(c).0, "blocked", "{:?}", e.task(c));
    assert!(
        e.task(c).1.starts_with(&format!("waits on task {a3} ")),
        "{:?}",
        e.task(c)
    );

    // Path 1: `forge retry` re-points b's after list at the new attempt,
    // but no longer releases b itself -- that takes the new attempt
    // actually landing.
    assert!(
        e.forge("ok.sh", &["retry", &a.to_string()])
            .status
            .success()
    );
    let a2: i64 = e
        .db()
        .query_row("SELECT MAX(id) FROM tasks", [], |r| r.get(0))
        .unwrap();
    let after: String = e
        .db()
        .query_row("SELECT after_json FROM tasks WHERE id=?1", [b], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(after, format!("[{a2}]"));
    assert_eq!(e.task(b).0, "blocked", "{:?}", e.task(b));

    // Capped at one claim so this call only runs a2, isolating the
    // release it fires (the moment a2 lands) from b then also being
    // picked up and run.
    let o = e.forge("ok.sh", &["work", "--once", "--max-tasks", "1"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(a2).0, "succeeded", "{:?}", e.task(a2));
    assert_eq!(e.task(b).0, "queued", "{:?}", e.task(b));
    assert_eq!(e.task(b).1, "");
    // c, waiting on the unrelated a3, is untouched by any of this.
    assert_eq!(e.task(c).0, "blocked", "{:?}", e.task(c));

    // Path 2: a direct edit of c's after list, re-pointed by hand at the
    // task that already landed above. Nothing re-evaluates c at edit
    // time -- the next claim loop's periodic scan is what catches it up.
    e.db()
        .execute(
            "UPDATE tasks SET after_json=?2 WHERE id=?1",
            rusqlite::params![c, format!("[{a2}]")],
        )
        .unwrap();
    assert_eq!(e.task(c).0, "blocked", "the edit alone changes nothing yet");

    // b, already queued from the step above, is the oldest claimable task
    // and takes this call's one slot; that is enough to prove the point --
    // the periodic scan still queues c before any claim is attempted.
    let o = e.forge("ok.sh", &["work", "--once", "--max-tasks", "1"]);
    let _ = o;
    assert_eq!(e.task(c).0, "queued", "{:?}", e.task(c));
    assert_eq!(e.task(c).1, "");
}

#[test]
fn a_retry_of_a_verified_task_starts_from_its_branch() {
    // 1 writes the answer and passes the checks; the reviewer demotes it.
    // The retry starts from 1's branch and only adds what the review asked.
    let e = Env::new();
    let o = run_wf(
        &e,
        "ok.sh",
        &[("FORGE2_CLAUDE_BIN_REVIEW", "reviewer-demote.sh")],
        "reviewed",
        "write 42",
    );
    let (state, reason, pushed) = e.task(1);
    assert_eq!(
        state,
        "blocked",
        "{reason}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(reason.starts_with("review demoted"), "{reason}");
    assert!(pushed);
    assert!(e.forge("ok.sh", &["retry", "1"]).status.success());
    let mut c = e.with_role("addfile.sh", "REVIEW", "reviewer-ok.sh");
    let o = c.args(["work", "--once"]).output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(
        err.contains("start    from task 1's verified branch forge/1-write-42 @"),
        "{err}"
    );
    assert_eq!(e.task(2).0, "succeeded", "{:?}", e.task(2));
    // The retry's branch carries 1's answer and its own addition.
    assert_eq!(
        origin_file(&e, "forge/2-write-42", "answer.txt").as_deref(),
        Some("42\n")
    );
    assert_eq!(
        origin_file(&e, "forge/2-write-42", "extra.txt").as_deref(),
        Some("extra\n")
    );
    let (base, start): (String, String) = e
        .db()
        .query_row(
            "SELECT t.base_sha, a.start_sha FROM tasks t JOIN attempts a ON a.task_id = t.id WHERE t.id = 2 AND a.attempt_no = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_ne!(
        base, start,
        "the first attempt began past the base, on 1's commits"
    );
    // The journal tells the retry what the reviewer found.
    let prompt = e.log_text(2, 1);
    assert!(
        prompt.contains("it stopped with: review demoted"),
        "{prompt}"
    );
}

#[test]
fn a_retry_of_a_question_whose_checks_already_passed_starts_from_its_branch() {
    // 1 commits an answer and the repository's checks pass on it, but the
    // agent asks a question instead of returning cleanly. The retry must
    // start from 1's branch, the same as a review demotion the operator
    // set aside, rather than rebuild from main and repeat 1's already
    // verified commit.
    let e = Env::new();
    let o = e.run("commitneedsinput.sh", &["--retries", "0"]);
    let (state, reason, pushed) = e.task(1);
    assert_eq!(
        state,
        "blocked",
        "{reason}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        reason.starts_with("needs input: Should ANSWER.txt"),
        "{reason}"
    );
    assert!(!pushed, "a plain question does not push the branch");
    assert!(e.forge("addfile.sh", &["retry", "1"]).status.success());
    let o = e.forge("addfile.sh", &["work", "--once"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(
        err.contains("start    from task 1's verified branch forge/1-write-42-to-answertxt @"),
        "{err}"
    );
    assert_eq!(e.task(2).0, "succeeded", "{:?}", e.task(2));
    // The retry's branch carries 1's answer and its own addition.
    assert_eq!(
        origin_file(&e, "forge/2-write-42-to-answertxt", "answer.txt").as_deref(),
        Some("42\n")
    );
    assert_eq!(
        origin_file(&e, "forge/2-write-42-to-answertxt", "extra.txt").as_deref(),
        Some("extra\n")
    );
    let (base, start): (String, String) = e
        .db()
        .query_row(
            "SELECT t.base_sha, a.start_sha FROM tasks t JOIN attempts a ON a.task_id = t.id WHERE t.id = 2 AND a.attempt_no = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_ne!(
        base, start,
        "the first attempt began past the base, on 1's commit"
    );
}

#[test]
fn a_retry_whose_merged_base_does_not_build_feeds_setup_to_the_coder() {
    // Task 1 lands. Something else lands on main afterward that the merge
    // takes in cleanly but that breaks the build: a fake `setup` check
    // fails only once that file is present. The retry of task 1 starts
    // from its verified branch, merges the new main in, and setup fails
    // before any attempt. That must not fail the task with nothing to
    // show for it: the coder sees the error and fixes it on the branch.
    let e = Env::new();
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\nanswer = [\"bash\", \"-c\", \"test -f answer.txt && grep -qx 42 answer.txt\"]\n\
         shell = [\"bash\", \"-n\", \"hello.sh\"]\n\
         setup = [\"bash\", \"-c\", \"if [ -f broken.txt ]; then echo 'broken.txt is present'; exit 1; fi; echo ok\"]\n",
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "add a setup check"]);

    let o = e.forge(
        "mergefix.sh",
        &[
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(reason.starts_with("landed main @ "), "{reason}");
    assert!(pushed);

    // Something else lands on main: a file that setup, unchanged, now rejects.
    git(&e.repo, &["fetch", "-q", "origin", "main"]);
    git(&e.repo, &["checkout", "-q", "-B", "advance", "origin/main"]);
    std::fs::write(e.repo.join("broken.txt"), "boom\n").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &[
            "commit",
            "-qm",
            "advance main with a change that breaks the build",
        ],
    );
    git(&e.repo, &["push", "-q", "origin", "advance:main"]);
    git(&e.repo, &["checkout", "-q", "main"]);
    git(&e.repo, &["branch", "-D", "advance"]);

    assert!(e.forge("mergefix.sh", &["retry", "1"]).status.success());
    let o = e.forge("mergefix.sh", &["work", "--once"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(
        err.contains("start    from task 1's verified branch forge/1-write-42 @"),
        "{err}"
    );
    assert!(err.contains("with the current base merged in"), "{err}");
    assert!(
        err.contains("the merged base does not build; code will see the error"),
        "{err}"
    );

    let (state, reason, pushed) = e.task(2);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(reason.starts_with("landed main @ "), "{reason}");
    assert!(pushed);
    assert_eq!(
        op_names(&e, 2),
        vec![
            ("clone".into(), true),
            ("setup".into(), false),
            ("repo-map".into(), true),
            ("verify".into(), true),
            ("integrate".into(), true),
            ("push".into(), true),
            ("land".into(), true)
        ]
    );
    // The coder saw the setup failure and fixed it, on top of task 1's answer.
    let prompt = e.log_text(2, 1);
    assert!(prompt.contains("does not build"), "{prompt}");
    assert_eq!(
        origin_file(&e, "main", "answer.txt").as_deref(),
        Some("42\n")
    );
    assert_eq!(origin_file(&e, "main", "broken.txt"), None);
}

#[test]
fn a_landing_on_the_reviewed_workflow_runs_assess_and_stores_the_row() {
    let e = Env::new();
    let mut c = e.cmd("ok.sh");
    for (role, fake) in [("REVIEW", "reviewer-ok.sh"), ("ASSESS", "assessor.sh")] {
        c.env(
            format!("FORGE2_CLAUDE_BIN_{role}"),
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fakes")
                .join(fake),
        );
    }
    let o = c
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--workflow",
            "reviewed",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(reason.starts_with("landed main @ "), "{reason}");
    assert!(pushed);
    let (score, findings_json, model, provider, cost_usd): (i64, String, String, String, f64) = e
        .db()
        .query_row(
            "SELECT score, findings_json, model, provider, cost_usd FROM assessments WHERE task_id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(score, 7);
    assert!(findings_json.contains("answer.txt"), "{findings_json}");
    assert!(findings_json.contains("notable"), "{findings_json}");
    assert!(!model.is_empty());
    assert_eq!(provider, "anthropic");
    assert_eq!(cost_usd, 0.02);
}

#[test]
fn an_assessed_landing_carries_its_score_and_finding_on_show_and_trace() {
    let e = Env::new();
    let mut c = e.cmd("ok.sh");
    for (role, fake) in [("REVIEW", "reviewer-ok.sh"), ("ASSESS", "assessor.sh")] {
        c.env(
            format!("FORGE2_CLAUDE_BIN_{role}"),
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fakes")
                .join(fake),
        );
    }
    let o = c
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--workflow",
            "reviewed",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(1).0, "succeeded");

    let show = String::from_utf8_lossy(&e.forge("ok.sh", &["show", "1"]).stdout).to_string();
    assert!(
        show.contains("assess     score 7/10, 1 finding(s)"),
        "{show}"
    );
    assert!(
        show.contains(
            "finding    notable answer.txt: the value 42 is a magic number with no explanation."
        ),
        "{show}"
    );

    let doc = e.trace_json(1);
    let assessment = &doc["assessment"];
    assert_eq!(assessment["score"], 7);
    assert_eq!(assessment["provider"], "anthropic");
    let findings = assessment["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["path"], "answer.txt");
    assert_eq!(findings[0]["severity"], "notable");
}

#[test]
fn a_landing_on_the_direct_workflow_does_not_run_assess() {
    let e = Env::new();
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--workflow",
            "direct",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(1).0, "succeeded");
    let n: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM assessments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}
