use super::*;

#[tokio::test]
async fn namespace_commits_are_rejected_even_when_later_deleted() {
    let (dir, base) = commit_fixture().await;
    let path = "tests/acceptance/shadow.sh";
    std::fs::create_dir_all(dir.path().join("tests/acceptance")).unwrap();
    std::fs::write(dir.path().join(path), "true").unwrap();
    crate::git::commit_all(dir.path(), "commit overlay")
        .await
        .unwrap();
    std::fs::remove_file(dir.path().join(path)).unwrap();
    crate::git::commit_all(dir.path(), "remove overlay")
        .await
        .unwrap();
    assert_eq!(
        crate::git::changed_paths(dir.path(), &base).await.unwrap(),
        vec!["real.txt"],
    );
    let mut cfg = test_cfg();
    cfg.namespace = vec!["tests/acceptance/".into()];
    let report = Reporter::new(false, None);
    let s = Subject {
        task_id: 1,
        repo: dir.path(),
        worktree: dir.path(),
        base_sha: &base,
        start_sha: &base,
        branch: "forge/1",
        cfg: &cfg,
        task_checks: &[],
        paths: &[],
        allow_protected: false,
        overlay_refs: &[],
        pending_main: None,
        sandbox: None,
        report: &report,
        logs_dir: dir.path(),
        scratch: None,
        plan_rows: true,
    };
    let outcome = Outcome {
        exit_code: Some(0),
        got_result: true,
        ..wrong_changes_outcome()
    };
    let v = verify_directive(Contract::Code, &s, &outcome)
        .await
        .unwrap();
    let row = v
        .checks
        .iter()
        .find(|c| c.name == "namespace-untouched")
        .unwrap();
    assert!(!row.ok);
    assert!(row.tail.contains(path), "{}", row.tail);
    assert_eq!(v.state, AttemptState::ChecksFailed);
}
