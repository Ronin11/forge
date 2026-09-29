//! Fixtures shared by the verification submodules' tests.

use super::*;
use std::path::PathBuf;

/// A tempdir with a base commit and a second commit that adds
/// `real.txt`, as an attempt's own work. Returns the directory and the
/// base sha, the attempt's `start_sha`.
pub(super) async fn commit_fixture() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    };
    run(&["init", "--quiet"]);
    run(&["config", "user.email", "a@a.com"]);
    run(&["config", "user.name", "a"]);
    std::fs::write(dir.path().join("base.txt"), "one\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "--quiet", "-m", "base"]);
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
    std::fs::write(dir.path().join("real.txt"), "two\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "--quiet", "-m", "attempt"]);
    (dir, base)
}

pub(super) fn test_cfg() -> Config {
    Config {
        shared_target: false,
        repo_path: PathBuf::new(),
        build_env: Default::default(),
        execution: Default::default(),
        checks: std::collections::BTreeMap::new(),
        fixable: std::collections::BTreeMap::new(),
        base_branch: "main".into(),
        push_remote: None,
        check_timeout_secs: 60,
        protected: vec![],
        namespace: vec![],
        egress: vec![],
        environment_deny: vec![],
        config_path: "forge.toml".into(),
    }
}

/// A structured result naming a file the attempt never touched instead
/// of the one it actually committed: the retired `changes-match-git`
/// rule would have failed this, exactly the false-claim-about-the-report
/// shape dev.home's qwen3-coder:30b hit on task 324. Every provider now
/// has its `changes[]` derived from git, so the wrong list is simply
/// replaced, never a reason to fail.
pub(super) fn wrong_changes_outcome() -> Outcome {
    Outcome {
        structured: Some(
            r#"{"schema_version":1,"summary":"did it","needs_input":null,"changes":[{"path":"wrong.txt","kind":"added"}],"checks_run":[],"claims":[]}"#
                .into(),
        ),
        ..Default::default()
    }
}
