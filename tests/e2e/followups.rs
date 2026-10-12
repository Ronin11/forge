//! End-to-end coverage for `src/mechanic.rs`, through the real binary:
//! a task that ends `Failed` gets a mechanic decision and, for a
//! recognized kind, a follow-up task queued behind it.

use crate::support::*;

/// (kind, retry_id, citations) for the sole decision `mechanic` recorded
/// on `task_id`.
fn mechanic_decision(e: &Env, task_id: i64) -> (String, Option<i64>, String) {
    e.db()
        .query_row(
            "SELECT kind, retry_id, citations FROM decisions WHERE task_id=?1 AND answered_by='mechanic'",
            [task_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

#[test]
fn an_l0_clean_tree_failure_is_retried_once_with_guidance_appended() {
    let e = Env::new();
    assert!(!e.run("dirty.sh", &["--retries", "0"]).status.success());
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "failed");
    assert!(reason.starts_with("L0 failed: clean-tree"), "{reason}");

    let (kind, retry_id, _) = mechanic_decision(&e, 1);
    assert_eq!(kind, "mechanic-clean-tree");
    let retry_id = retry_id.expect("clean-tree is retried");
    let (retry_state, retry_task): (String, String) = e
        .db()
        .query_row(
            "SELECT state, task FROM tasks WHERE id=?1",
            [retry_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(retry_state, "queued");
    assert!(
        retry_task.contains("git status --porcelain must print nothing"),
        "{retry_task}"
    );
}

#[test]
fn a_ratchet_check_failure_is_refiled_with_guidance_and_no_retry_of() {
    let e = Env::new();
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\ntest = [\"bash\", \"-c\", \"echo 'src/big.rs: 1600 lines exceeds ceiling 1500'; echo 'Split the file the way src/store/ and src/cli/ were split.'; echo failures:; echo '    tracked_rust_files_stay_within_their_line_limits'; exit 1\"]\n",
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "a test check that ratchets"]);

    assert!(!e.run("ok.sh", &["--retries", "0"]).status.success());
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "failed");
    assert!(reason.starts_with("L1 failed: test"), "{reason}");

    let (kind, retry_id, citations) = mechanic_decision(&e, 1);
    assert_eq!(kind, "mechanic-ratchet");
    assert!(citations.contains("supersedes task 1"), "{citations}");
    let refiled_id = retry_id.expect("a ratchet refiles a follow-up task");
    let (refiled_state, refiled_task, retry_of): (String, String, Option<i64>) = e
        .db()
        .query_row(
            "SELECT state, task, retry_of FROM tasks WHERE id=?1",
            [refiled_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(refiled_state, "queued");
    assert_eq!(retry_of, None, "a refile is a fresh lineage, not a retry");
    assert!(
        refiled_task.contains("src/big.rs is at its line ceiling (1600 exceeds 1500)"),
        "{refiled_task}"
    );
    assert!(
        refiled_task.contains("new module or function"),
        "{refiled_task}"
    );
}

#[test]
fn a_failure_of_no_recognized_kind_is_retried_once_on_the_strong_lane_then_raised() {
    let e = Env::new();
    let strong = |args: &[&str]| {
        let mut c = e.cmd("ok.sh");
        c.env("FORGE_STRONG_LANE", "1").args(args);
        c.output().expect("forge")
    };
    assert!(
        !strong(&[
            "run",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--no-land",
            "--retries",
            "0",
            "--check",
            "grep -qx 43 answer.txt",
        ])
        .status
        .success()
    );
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "failed");

    // The agent ran and its checks failed in no mechanical way: once on
    // the strong model, pinned, with the branch as the starting tree.
    let (kind, retry_id, citations) = mechanic_decision(&e, 1);
    assert_eq!(kind, "mechanic-strong-lane");
    let retry_id = retry_id.expect("the strong lane files a retry");
    assert!(citations.contains(&reason), "{citations}");
    let (retry_state, model, model_source, retry_of): (String, String, String, Option<i64>) = e
        .db()
        .query_row(
            "SELECT state, model, model_source, retry_of FROM tasks WHERE id=?1",
            [retry_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        (retry_state.as_str(), model.as_str(), model_source.as_str()),
        ("queued", "opus", "flag")
    );
    assert_eq!(retry_of, Some(1));

    // The retry fails the same way: the lane is spent, the operator's turn.
    let out = strong(&["work", "--once"]);
    let (state, reason, _) = e.task(retry_id);
    assert_eq!(
        state,
        "failed",
        "{reason}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (kind, third, _) = mechanic_decision(&e, retry_id);
    assert_eq!(kind, "mechanic-block");
    assert!(third.is_none(), "no third attempt by the mechanic");
}
