use crate::support::*;

#[test]
fn fold_catches_up_with_remote_and_next_verification_overlays_it() {
    let e = Env::new();
    tdd_repo(&e);
    let other = e.repo.parent().unwrap().join("suite-editor");
    git(
        &e.repo,
        &[
            "clone",
            "-q",
            e.repo.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    git(&other, &["config", "user.name", "Other"]);
    git(&other, &["config", "user.email", "other@example.com"]);
    git(&other, &["checkout", "--orphan", "forge-verify"]);
    git(&other, &["rm", "-rf", "."]);
    git(
        &other,
        &["commit", "--allow-empty", "-qm", "standing suite"],
    );
    git(
        &other,
        &["push", "-q", e.origin.to_str().unwrap(), "forge-verify"],
    );
    git(&e.repo, &["fetch", "origin", "forge-verify:forge-verify"]);
    let o = e
        .with_role("ok.sh", "TESTS", "testwriter.sh")
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--workflow",
            "tdd",
            "--no-land",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    std::fs::create_dir_all(other.join("tests/acceptance")).unwrap();
    std::fs::write(other.join("tests/acceptance/remote.sh"), "exit 1\n").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-qm", "remote suite addition"]);
    git(
        &other,
        &["push", "-q", e.origin.to_str().unwrap(), "forge-verify"],
    );
    let remote = git(&other, &["rev-parse", "HEAD"]);

    let o = e.forge("ok.sh", &["land", "1"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        origin_file(&e, "forge-verify", "tests/acceptance/remote.sh").as_deref(),
        Some("exit 1\n")
    );
    assert_eq!(
        git(&e.repo, &["merge-base", "forge-verify", &remote]),
        remote
    );
    assert_eq!(
        git(
            &e.repo,
            &["show", "forge-verify:tests/acceptance/remote.sh"]
        ),
        "exit 1"
    );

    assert!(origin_file(&e, "forge-verify", "tests/acceptance/answer.sh").is_some());
    assert_ne!(git(&e.repo, &["rev-parse", "forge-verify"]), remote);

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
    assert!(
        !o.status.success(),
        "the remote test must reject the next task"
    );
    assert_eq!(e.task(2).0, "failed");
    let doc = e.trace_json(2);
    assert!(
        doc["attempts"].as_array().unwrap().iter().any(|a| {
            a["inputs"]["overlay_refs"]
                .to_string()
                .contains(&git(&e.repo, &["rev-parse", "forge-verify"]))
        }),
        "{doc}"
    );
}
