use crate::support::*;
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
        .cmd("slowaddfile.sh")
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
    let mut c = e.with_role("slowfeedback.sh", "TESTS", "testwriter.sh");
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
        e.log_text(1, 3).contains("verification fails"),
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
    let doc: serde_json::Value = e.trace_json("4");
    assert_eq!(doc["task"]["state"], "queued", "{doc}");
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
    // A rerouted dependent no longer waits on a failed task: it leaves the human queue.
    let reqs: serde_json::Value = e.requests_json();
    assert!(reqs.as_array().unwrap().is_empty(), "{reqs}");
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
    assert_eq!(e.task(5).0, "queued", "{:?}", e.task(5));
    let after: String = e
        .db()
        .query_row("SELECT after_json FROM tasks WHERE id=5", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, "[6]");
}
