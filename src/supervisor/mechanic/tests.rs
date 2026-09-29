//! A fixture per kind: a real (throwaway) git repository and store, a
//! failed task with the shape that kind's classifier looks for, and
//! `mechanic::act` run against it directly — the same call `worker::drive`
//! makes once a task ends `TaskState::Failed`.

use super::*;
use crate::ctx::Paths;
use crate::store::{Attempt, AttemptState, FinishAttempt, Store};

fn run(dir: &Path, args: &[&str]) {
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

/// A repository with one commit (`forge.toml` declaring a `test` check
/// that always passes, plus `base.txt`) and, on top, a second commit
/// standing in for the coder's own branch. Returns the directory and the
/// base commit's sha; `HEAD` is the "branch".
fn repo_fixture() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    run(dir.path(), &["init", "--quiet", "--initial-branch=main"]);
    run(dir.path(), &["config", "user.email", "a@a.com"]);
    run(dir.path(), &["config", "user.name", "a"]);
    std::fs::write(
        dir.path().join("forge.toml"),
        "[checks]\ntest = [\"true\"]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("base.txt"), "one\n").unwrap();
    run(dir.path(), &["add", "."]);
    run(dir.path(), &["commit", "--quiet", "-m", "base"]);
    let base = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/branch.rs"), "// two\n").unwrap();
    run(dir.path(), &["add", "."]);
    run(dir.path(), &["commit", "--quiet", "-m", "branch"]);
    (dir, base)
}

fn fixture_forge(home: &std::path::Path) -> Forge {
    let paths = Paths {
        worktrees: home.join("worktrees"),
        logs: home.join("logs"),
        home: home.to_path_buf(),
    };
    std::fs::create_dir_all(&paths.worktrees).unwrap();
    std::fs::create_dir_all(&paths.logs).unwrap();
    let store = Store::open(&home.join("forge.db")).unwrap();
    Forge::open_with(paths, store).unwrap()
}

/// Inserts a task that already ended `Failed` with `reason`, its worktree
/// and repo both `repo`.
fn fixture_task(f: &Forge, repo: &std::path::Path, base_sha: &str, reason: &str) -> Task {
    let mut t = Task {
        repo: repo.display().to_string(),
        task: "the original task text".into(),
        base_branch: "main".into(),
        base_sha: base_sha.to_string(),
        branch: "forge/1".into(),
        worktree: repo.display().to_string(),
        model: "m".into(),
        max_turns: 10,
        max_attempts: 2,
        timeout_secs: 60,
        state: TaskState::Failed,
        reason: reason.to_string(),
        created_at: crate::unix_now(),
        finished_at: Some(crate::unix_now()),
        workflow: "direct".into(),
        land: true,
        ..Default::default()
    };
    t.id = f.store.insert_task(&t).unwrap();
    f.store.update_task(&t).unwrap();
    t
}

/// Adds a `ChecksFailed` attempt on `t` carrying `verdict` (empty when the
/// kind under test needs none, e.g. a landing conflict or a turn cap).
fn fail_checks(f: &Forge, task_id: i64, verdict: Vec<CheckResult>) {
    let a = Attempt {
        task_id,
        attempt_no: 1,
        step: "tests".into(),
        step_seq: 2,
        start_sha: "x".into(),
        state: AttemptState::Running,
        started_at: 1,
        ..Default::default()
    };
    let id = f.store.insert_attempt(&a).unwrap();
    f.store
        .finish_attempt(&FinishAttempt {
            id,
            state: AttemptState::ChecksFailed,
            reason: "L1 failed: test".into(),
            verdict_json: serde_json::to_string(&verdict).unwrap(),
            outputs_json: "{}".into(),
            early_signals: "[]".into(),
            early_near: "[]".into(),
            end_sha: "x".into(),
            ..Default::default()
        })
        .unwrap();
}

fn only_decision(f: &Forge, task_id: i64) -> crate::store::Decision {
    let mut ds = f.store.decisions_in_lineage(task_id).unwrap();
    assert_eq!(
        ds.len(),
        1,
        "expected exactly one decision on task {task_id}"
    );
    ds.remove(0)
}

#[tokio::test]
async fn a_landing_conflict_retries_once_through_the_integrator() {
    let (repo, base) = repo_fixture();
    let f = fixture_forge(repo.path());
    let t = fixture_task(
        &f,
        repo.path(),
        &base,
        "landing failed after 2 attempt(s): main moved to a6cb6bd; conflicts in base.txt; the verified branch is pushed for a human",
    );
    act(&f, t.id).await.unwrap();
    let d = only_decision(&f, t.id);
    assert_eq!(d.kind, Kind::LandingConflict.as_str());
    let retried = f.store.task(d.retry_id.unwrap()).unwrap().unwrap();
    assert_eq!(retried.retry_of, Some(t.id));
    assert_eq!(retried.task, t.task, "no guidance appended for this kind");
}

#[tokio::test]
async fn a_landing_conflict_may_retry_twice_then_blocks() {
    let (repo, base) = repo_fixture();
    let f = fixture_forge(repo.path());
    let reason = "landing failed after 2 attempt(s): main moved to a6cb6bd; conflicts in base.txt; the verified branch is pushed for a human";
    let first = fixture_task(&f, repo.path(), &base, reason);
    act(&f, first.id).await.unwrap();
    let retried_once = only_decision(&f, first.id).retry_id.unwrap();

    let mut second = f.store.task(retried_once).unwrap().unwrap();
    second.state = TaskState::Failed;
    second.reason = reason.to_string();
    f.store.update_task(&second).unwrap();
    act(&f, second.id).await.unwrap();
    // `decisions_in_lineage` walks up from the task given, so the whole
    // chain so far is only visible from its latest member.
    let decisions = f.store.decisions_in_lineage(second.id).unwrap();
    assert_eq!(decisions.len(), 2);
    assert!(
        decisions
            .iter()
            .all(|d| d.kind == Kind::LandingConflict.as_str())
    );
    let retried_twice = decisions.last().unwrap().retry_id.unwrap();

    let mut third = f.store.task(retried_twice).unwrap().unwrap();
    third.state = TaskState::Failed;
    third.reason = reason.to_string();
    f.store.update_task(&third).unwrap();
    act(&f, third.id).await.unwrap();
    let decisions = f.store.decisions_in_lineage(third.id).unwrap();
    assert_eq!(
        decisions.len(),
        3,
        "a third landing conflict blocks instead"
    );
    let last = decisions.last().unwrap();
    assert_eq!(last.kind, BLOCK_KIND);
    assert!(last.answer.contains("already tried"));
    let after = f.store.task(third.id).unwrap().unwrap();
    assert_eq!(
        after.state,
        TaskState::Failed,
        "a block leaves the task as it was"
    );
}

#[tokio::test]
async fn a_turn_cap_with_commits_retries_with_doubled_max_turns() {
    let (repo, base) = repo_fixture();
    let f = fixture_forge(repo.path());
    let t = fixture_task(
        &f,
        repo.path(),
        &base,
        "ran out of turns after committing; the checks fail: L1 failed: test (after 2 attempt(s))",
    );
    act(&f, t.id).await.unwrap();
    let d = only_decision(&f, t.id);
    assert_eq!(d.kind, Kind::TurnCap.as_str());
    let retried = f.store.task(d.retry_id.unwrap()).unwrap().unwrap();
    assert_eq!(retried.max_turns, t.max_turns * 2);
}

#[tokio::test]
async fn an_l0_clean_tree_failure_retries_with_guidance_appended() {
    let (repo, base) = repo_fixture();
    let f = fixture_forge(repo.path());
    let t = fixture_task(
        &f,
        repo.path(),
        &base,
        "L0 failed: clean-tree (after 1 attempt(s))",
    );
    act(&f, t.id).await.unwrap();
    let d = only_decision(&f, t.id);
    assert_eq!(d.kind, Kind::CleanTree.as_str());
    let retried = f.store.task(d.retry_id.unwrap()).unwrap().unwrap();
    assert!(retried.task.starts_with(&t.task));
    assert!(
        retried
            .task
            .contains("git status --porcelain must print nothing")
    );
}

#[tokio::test]
async fn a_ratchet_failure_refiles_a_fresh_task_instead_of_retrying() {
    let (repo, base) = repo_fixture();
    let f = fixture_forge(repo.path());
    let t = fixture_task(
        &f,
        repo.path(),
        &base,
        "L1 failed: test (after 2 attempt(s))",
    );
    fail_checks(
        &f,
        t.id,
        vec![CheckResult {
            level: "L1".into(),
            name: "test".into(),
            ok: false,
            tail: "src/big.rs: 1600 lines exceeds ceiling 1500\nSplit the file the way src/store/ and src/cli/ were split.".into(),
            failing_tests: vec!["tracked_rust_files_stay_within_their_line_limits".into()],
            ..Default::default()
        }],
    );
    act(&f, t.id).await.unwrap();
    let d = only_decision(&f, t.id);
    assert_eq!(d.kind, Kind::Ratchet.as_str());
    assert!(d.citations.contains(&format!("supersedes task {}", t.id)));
    let refiled = f.store.task(d.retry_id.unwrap()).unwrap().unwrap();
    assert_eq!(refiled.retry_of, None, "a refile is a fresh lineage");
    assert!(refiled.task.contains("src/big.rs is at its line ceiling"));
    assert!(refiled.task.contains("new module or function"));
}

#[tokio::test]
async fn a_load_flake_retries_once_when_untouched_and_green_on_base() {
    let (repo, base) = repo_fixture();
    let f = fixture_forge(repo.path());
    let t = fixture_task(
        &f,
        repo.path(),
        &base,
        "L1 failed: test (after 1 attempt(s))",
    );
    fail_checks(
        &f,
        t.id,
        vec![CheckResult {
            level: "L1".into(),
            name: "test".into(),
            ok: false,
            tail: "failures:\n    worker::window_hold_waits\n".into(),
            failing_tests: vec!["worker::window_hold_waits".into()],
            ..Default::default()
        }],
    );
    // `branch.txt` is the only file the branch touched; the failing test's
    // name shares nothing with it, and the fixture's `test` check is
    // `true`, always green — including on the base archive.
    act(&f, t.id).await.unwrap();
    let d = only_decision(&f, t.id);
    assert_eq!(d.kind, Kind::LoadFlake.as_str());
    let retried = f.store.task(d.retry_id.unwrap()).unwrap().unwrap();
    assert_eq!(retried.retry_of, Some(t.id));
}

#[tokio::test]
async fn a_failing_test_the_branch_touched_is_not_a_load_flake() {
    let (repo, base) = repo_fixture();
    let f = fixture_forge(repo.path());
    let t = fixture_task(
        &f,
        repo.path(),
        &base,
        "L1 failed: test (after 1 attempt(s))",
    );
    fail_checks(
        &f,
        t.id,
        vec![CheckResult {
            level: "L1".into(),
            name: "test".into(),
            ok: false,
            tail: "failures:\n    branch::a_test\n".into(),
            failing_tests: vec!["branch::a_test".into()],
            // The branch's own change is `src/branch.rs`; this test's name
            // shares that stem, so it counts as touched.
            ..Default::default()
        }],
    );
    act(&f, t.id).await.unwrap();
    let d = only_decision(&f, t.id);
    assert_eq!(d.kind, BLOCK_KIND, "touched by the branch, not a flake");
}

#[tokio::test]
async fn an_unrecognized_failure_blocks_with_a_decision_and_leaves_the_task_failed() {
    let (repo, base) = repo_fixture();
    let f = fixture_forge(repo.path());
    let t = fixture_task(&f, repo.path(), &base, "agent exit 1 (after 1 attempt(s))");
    act(&f, t.id).await.unwrap();
    let d = only_decision(&f, t.id);
    assert_eq!(d.kind, BLOCK_KIND);
    assert!(d.retry_id.is_none());
    let after = f.store.task(t.id).unwrap().unwrap();
    assert_eq!(after.state, TaskState::Failed);
}

#[tokio::test]
async fn an_operation_failure_blocks_with_a_decision_like_any_other_unrecognized_failure() {
    let (repo, base) = repo_fixture();
    let f = fixture_forge(repo.path());
    let t = fixture_task(&f, repo.path(), &base, "operation setup failed: exit 1");
    act(&f, t.id).await.unwrap();
    let d = only_decision(&f, t.id);
    assert_eq!(d.kind, BLOCK_KIND);
    assert!(d.retry_id.is_none());
    let after = f.store.task(t.id).unwrap().unwrap();
    assert_eq!(after.state, TaskState::Failed);
}

#[tokio::test]
async fn an_internal_error_failure_blocks_with_a_decision_like_any_other_unrecognized_failure() {
    let (repo, base) = repo_fixture();
    let f = fixture_forge(repo.path());
    let t = fixture_task(&f, repo.path(), &base, "error: some internal fault");
    act(&f, t.id).await.unwrap();
    let d = only_decision(&f, t.id);
    assert_eq!(d.kind, BLOCK_KIND);
    assert!(d.retry_id.is_none());
    let after = f.store.task(t.id).unwrap().unwrap();
    assert_eq!(after.state, TaskState::Failed);
}
