use crate::support::*;
use std::process::Command;

fn origin_sha(e: &Env, branch: &str) -> String {
    let o = Command::new("git")
        .args([
            "--git-dir",
            e.origin.to_str().unwrap(),
            "rev-parse",
            "--verify",
            "--quiet",
        ])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn show(e: &Env, id: i64) -> String {
    String::from_utf8_lossy(&e.forge("ok.sh", &["show", &id.to_string()]).stdout).to_string()
}

#[test]
fn a_task_whose_second_attempt_would_cross_the_cap_ends_capped_with_its_branch_and_handoff() {
    let e = Env::new();
    let o = e.run("costnocommit.sh", &["--budget", "0.015", "--retries", "3"]);
    assert!(!o.status.success());
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "capped", "{reason}");
    assert!(
        reason.starts_with("$0.01 of $0.01"),
        "the reason names spent and cap: {reason}"
    );
    assert!(pushed, "the branch stays pushed");
    assert!(
        e.origin_branches().contains("forge/1-"),
        "{}",
        e.origin_branches()
    );
    let show = String::from_utf8_lossy(&e.forge("ok.sh", &["show", "1"]).stdout).to_string();
    assert!(
        show.contains("left no commits past main"),
        "forge show says it left nothing to adopt: {show}"
    );
    assert_eq!(
        e.attempts(1).len(),
        1,
        "the second attempt was never started: the cap is not overshot"
    );
    let (session, handoff): (String, String) = e
        .db()
        .query_row(
            "SELECT session_id, handoff FROM tasks WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(session, "sess-wrong-1");
    assert!(handoff.contains("Handoff:"), "{handoff}");
    assert!(handoff.contains("write 42 to answer.txt"), "{handoff}");
}

#[test]
fn a_capped_task_is_listed_and_counted_apart_from_failed() {
    let e = Env::new();
    let o = e.run("costnocommit.sh", &["--budget", "0.015", "--retries", "3"]);
    assert!(!o.status.success());
    let o = e.forge("ok.sh", &["log", "--state", "capped", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
    assert_eq!(rows[0]["state"], "capped");
    let o = e.forge("ok.sh", &["log", "--state", "failed", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(rows.as_array().unwrap().is_empty(), "{rows}");
    let o = e.forge("ok.sh", &["stats", "--json"]);
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let w = &doc["workflows"][0];
    assert_eq!(w["capped"], 1, "{w}");
    assert_eq!(w["failed"], 0, "{w}");
}

#[test]
fn a_capped_task_whose_commits_pass_as_they_stand_lands_them_with_no_agent() {
    let e = Env::new();
    // The coder commits the right answer and dies at its turn cap; the
    // retry it would get is past the task's budget.
    let o = e.forge(
        "commitnoevidence.sh",
        &[
            "run",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--budget",
            "0.015",
            "--retries",
            "3",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(reason.starts_with("landed main @ "), "{reason}");
    assert_eq!(e.attempts(1).len(), 1, "no agent ran after the cap");
    assert_eq!(
        origin_file(&e, "main", "answer.txt").as_deref(),
        Some("42\n"),
        "the capped task's commit is on main"
    );
    let branch: String = e
        .db()
        .query_row("SELECT branch FROM tasks WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        origin_sha(&e, "main"),
        origin_sha(&e, &branch),
        "main fast-forwarded to the capped task's branch"
    );
    let ops = op_names(&e, 1);
    assert!(
        ops.contains(&("verify".to_string(), true)) && ops.contains(&("land".to_string(), true)),
        "{ops:?}"
    );
}

#[test]
fn a_capped_task_whose_commits_pass_is_held_for_a_human_with_no_land() {
    let e = Env::new();
    let o = e.run(
        "commitnoevidence.sh",
        &["--budget", "0.015", "--retries", "3"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(reason.starts_with("$0.01 of $0.01"), "{reason}");
    assert!(
        reason.contains("1 commit(s) past main pass the checks as they stand"),
        "{reason}"
    );
    assert!(reason.contains("forge land 1"), "{reason}");
    assert!(pushed, "the held branch is pushed");
    assert_eq!(origin_sha(&e, "main"), "", "nothing landed");
}

#[test]
fn a_capped_task_whose_commits_fail_as_they_stand_blocks_with_the_check_lines() {
    let e = Env::new();
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\nanswer = [\"bash\", \"-c\", \"echo answer.txt holds $(cat answer.txt), want 42; grep -qx 42 answer.txt\"]\n",
    )
    .unwrap();
    git(
        &e.repo,
        &["commit", "-qam", "say what the answer check saw"],
    );
    let o = e.run("wrongsession.sh", &["--budget", "0.015", "--retries", "3"]);
    assert!(!o.status.success());
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "blocked", "{reason}");
    assert!(reason.starts_with("$0.01 of $0.01"), "{reason}");
    assert!(
        reason.contains("1 commit(s) past main fail check answer as they stand"),
        "{reason}"
    );
    assert!(
        reason.contains("answer.txt holds 41, want 42"),
        "the failing check's lines are in the reason: {reason}"
    );
    assert!(
        reason.contains(&format!("forge adopt {} forge/1-", e.repo.display())),
        "{reason}"
    );
    assert!(pushed, "the branch is pushed for whoever picks it up");
    assert!(e.origin_branches().contains("forge/1-"));
    assert_eq!(e.attempts(1).len(), 1, "no agent ran after the cap");
    assert_eq!(origin_sha(&e, "main"), "", "nothing landed");
}

#[test]
fn forge_show_for_a_capped_task_counts_the_commits_it_left_and_names_forge_adopt() {
    let e = Env::new();
    // A dirty tree is never judged: the commits stay for a human.
    let o = e.run("commitdirty.sh", &["--budget", "0.015", "--retries", "3"]);
    assert!(!o.status.success());
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "capped", "{reason}");
    assert!(pushed, "{reason}");
    assert!(
        reason.contains("left 1 commit(s) past main and a dirty tree"),
        "{reason}"
    );
    let show = show(&e, 1);
    assert!(
        show.contains("left 1 commit(s) on forge/1-"),
        "forge show counts the commits: {show}"
    );
    assert!(
        show.contains(&format!("run forge adopt {} forge/1-", e.repo.display())),
        "forge show says what to do next: {show}"
    );
}
