use super::*;

#[tokio::test]
async fn verify_integration_refuses_a_forge_toml_change_unless_allow_protected() {
    let (dir, base) = commit_fixture().await;
    std::fs::write(
        dir.path().join("forge.toml"),
        "[checks]\nshell = [\"true\"]\n",
    )
    .unwrap();
    crate::git::commit_all(dir.path(), "merge carrying a forge.toml change")
        .await
        .unwrap();
    let cfg = test_cfg();
    let report = Reporter::new(false, None);
    let subject = |allow_protected: bool| Subject {
        task_id: 1,
        repo: dir.path(),
        worktree: dir.path(),
        base_sha: &base,
        start_sha: &base,
        branch: "forge/1",
        cfg: &cfg,
        task_checks: &[],
        paths: &[],
        allow_protected,
        overlay_refs: &[],
        pending_main: None,
        sandbox: None,
        report: &report,
        logs_dir: dir.path(),
        scratch: None,
        plan_rows: true,
    };
    let v = verify_integration(&subject(false)).await.unwrap();
    assert_eq!(
        v.checks
            .iter()
            .find(|c| c.name == "forge.toml-untouched")
            .map(|c| c.ok),
        Some(false),
        "{:?}",
        v.checks
    );
    assert_eq!(v.reason, "L0 failed: forge.toml-untouched");

    let v = verify_integration(&subject(true)).await.unwrap();
    assert_eq!(
        v.checks
            .iter()
            .find(|c| c.name == "forge.toml-untouched")
            .map(|c| c.ok),
        Some(true),
        "--allow-protected must lift the rule same as any other protected path: {:?}",
        v.checks
    );
}
