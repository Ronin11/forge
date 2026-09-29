use crate::support::*;

#[test]
fn a_retry_starts_clean_after_verification_stages_a_hidden_test_and_fails() {
    let e = Env::new();
    let command = "test -f tests/acceptance/answer.sh && git add tests/acceptance/answer.sh && bash tests/acceptance/answer.sh && test -f extra.txt";
    std::fs::write(e.repo.join("forge.toml"), format!(
        "[checks]\ntest = [\"bash\", \"-c\", {}]\n[verify]\nnamespace = [\"tests/acceptance/\"]\n",
        serde_json::to_string(command).unwrap(),
    )).unwrap();
    git(&e.repo, &["commit", "-qam", "verification stages overlay"]);
    let base_branch = git(&e.repo, &["branch", "--show-current"]);
    git(&e.repo, &["checkout", "-qb", "forge-verify"]);
    std::fs::create_dir_all(e.repo.join("tests/acceptance")).unwrap();
    std::fs::write(
        e.repo.join("tests/acceptance/answer.sh"),
        "grep -qx 42 answer.txt\n",
    )
    .unwrap();
    git(&e.repo, &["add", "tests/acceptance/answer.sh"]);
    git(&e.repo, &["commit", "-qm", "hidden verification"]);
    git(&e.repo, &["checkout", &base_branch]);
    let o = run_wf(&e, "ok.sh", &[], "direct", "write answer then extra");
    assert!(!o.status.success());
    let attempts = e.attempts(1);
    assert_eq!(check(&attempts[0].4, "L1", "test"), Some(false));
    let prior = e.home.join("worktrees/1");
    assert!(!prior.join("tests/acceptance").exists());
    assert_eq!(git(&prior, &["status", "--porcelain"]), "");
    let tip = git(&prior, &["rev-parse", "HEAD"]);
    assert!(e.forge("ok.sh", &["retry", "1"]).status.success());
    let o = e.forge("retry-clean.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(2).0, "succeeded", "{:?}", e.task(2));
    let start: String = e.db().query_row(
        "SELECT start_sha FROM attempts WHERE task_id=2 AND step='code' ORDER BY attempt_no LIMIT 1",
        [], |r| r.get(0),
    ).unwrap();
    assert_eq!(
        start, tip,
        "retry inherited the predecessor's implementation"
    );
    let retry = e.home.join("worktrees/2");
    assert!(!retry.join("tests/acceptance").exists());
    assert_eq!(git(&retry, &["status", "--porcelain"]), "");
    assert_eq!(git(&retry, &["ls-files", "tests/acceptance"]), "");
}
