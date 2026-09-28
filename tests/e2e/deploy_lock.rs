use crate::support::*;

#[test]
fn two_deploy_run_calls_on_one_target_never_overlap_their_method() {
    // REVIEW-4 E3-12: nothing serialised deploys of any method but
    // deploy-self, so two deploys landing on one target at once could both
    // rsync --delete into the same directory with the last to finish
    // winning. deploy::run now holds an exclusive flock on
    // FORGE_HOME/deploys/<project>-<target>.lock for all of run, so a
    // second deploy started while the first is still running its method
    // (here, sleeping in it) waits for the first to finish entirely before
    // its own method starts.
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();

    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo_s],
        )
        .status
        .success()
    );

    let dest = e._dir.path().join("remote");
    let log = e._dir.path().join("deploy-order.log");

    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo_s,
            "--method",
            "deploy-command",
            "--arg",
            "host=local",
            "--arg",
            &format!("dest={}", dest.to_str().unwrap()),
            "--arg",
            &format!("logfile={}", log.to_str().unwrap()),
            "--arg",
            r#"command=echo "start $(cat which.txt)" >> "$FORGE_ARG_LOGFILE"; sleep 1; echo "end $(cat which.txt)" >> "$FORGE_ARG_LOGFILE""#,
            "--check",
            "true",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // Two commits, distinguished only by which.txt (sized differently, so
    // rsync's quick check by size can never mistake one for the other
    // regardless of mtime), so the log can tell which deploy's method wrote
    // each line; both pass their check.
    std::fs::write(e.repo.join("which.txt"), "AAAAA\n").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "a"]);
    let sha_a = git(&e.repo, &["rev-parse", "HEAD"]);

    std::fs::write(e.repo.join("which.txt"), "B\n").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "b"]);
    let sha_b = git(&e.repo, &["rev-parse", "HEAD"]);

    // Spawned together: whichever wins the lock runs its method (a one
    // second sleep between its start and end log lines) while the other
    // blocks acquiring the same lock, rather than racing it into the same
    // destination.
    let mut c1 = e.cmd("ok.sh");
    c1.args(["deploy", "demo", "prod", "--sha", &sha_a]);
    let mut c2 = e.cmd("ok.sh");
    c2.args(["deploy", "demo", "prod", "--sha", &sha_b]);
    let child1 = c1.spawn().unwrap();
    let child2 = c2.spawn().unwrap();
    let outputs = [
        child1.wait_with_output().unwrap(),
        child2.wait_with_output().unwrap(),
    ];
    for o in &outputs {
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }

    let lines: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 4, "{lines:?}");
    // One run's method entirely finishes ("start X" then "end X") before
    // the other's method starts; a race would instead interleave them
    // ("start A", "start B", "end A", "end B" or similar).
    let which = |line: &str| line.rsplit(' ').next().unwrap().to_string();
    assert_eq!(lines[0], format!("start {}", which(&lines[0])));
    assert_eq!(lines[1], format!("end {}", which(&lines[1])));
    assert_eq!(which(&lines[0]), which(&lines[1]), "{lines:?}");
    assert_eq!(lines[2], format!("start {}", which(&lines[2])));
    assert_eq!(lines[3], format!("end {}", which(&lines[3])));
    assert_eq!(which(&lines[2]), which(&lines[3]), "{lines:?}");
    assert_ne!(which(&lines[0]), which(&lines[2]), "{lines:?}");
}
