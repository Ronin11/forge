//! REVIEW-3 2.1#5: a later landing round used to push the task branch
//! before the base had actually landed, so a round that had to merge a
//! base that kept moving could strand it on a commit the remote never
//! saw. This drives `integrate` through exactly that shape: the base
//! moves twice while the merged-tree checks run, the second move
//! conflicting, and the task still lands once the coder resolves it.

use crate::support::*;
use std::process::Command;
use std::time::Duration;

#[test]
fn the_base_moving_twice_during_merged_tree_verification_conflicts_once_and_still_lands() {
    let e = Env::new();
    // "slow" only sleeps once `moved.txt` is in the tree being checked:
    // fast for the coder's own branch, slow for landing's merged tree,
    // which is exactly the window a second, conflicting push needs.
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\nanswer = [\"bash\", \"-c\", \"test -s answer.txt\"]\nshell = [\"bash\", \"-n\", \"hello.sh\"]\nslow = [\"bash\", \"-c\", \"test -f moved.txt && sleep 2 || true\"]\n",
    )
    .unwrap();
    git(
        &e.repo,
        &["commit", "-qam", "any answer, slow once main moved"],
    );

    let child = e
        .cmd("echoanswer.sh")
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 99 to answer.txt",
            "--retries",
            "1",
        ])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

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

    // First move: harmless on its own, but it puts `moved.txt` in the
    // merged tree, so landing's own re-verification of it is slow.
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
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    git(&other, &["config", "user.name", "Other"]);
    git(&other, &["config", "user.email", "other@example.com"]);
    std::fs::write(other.join("moved.txt"), "moved\n").unwrap();
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "-qm", "main moved once"]);
    git(
        &other,
        &["push", "-q", e.origin.to_str().unwrap(), "main:main"],
    );

    // Once the branch's own (fast) verify has passed, round 0 of landing
    // is about to fetch this base and merge it in; a short, fixed margin
    // then puts us inside its now-slow re-verification, with two full
    // seconds still to spare before it tries to push.
    assert!(
        wait_until(
            || op_names(&e, 1).iter().any(|(n, ok)| n == "verify" && *ok),
            Duration::from_secs(10)
        ),
        "the branch never verified"
    );
    std::thread::sleep(Duration::from_millis(300));

    // Second move: conflicts with the coder's own answer.txt.
    std::fs::write(other.join("answer.txt"), "77\n").unwrap();
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "-qm", "main moved again, conflicting"]);
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
    assert!(err.contains("conflicts in"), "{err}");
    assert_eq!(e.attempts(1).len(), 2, "the coder ran once more to merge");
    assert!(
        e.log_text(1, 2).contains("git merge forge/main"),
        "the coder was told how"
    );
    assert_eq!(
        origin_file(&e, "main", "answer.txt").as_deref(),
        Some("99\n"),
        "the coder's own answer landed, not the conflicting one"
    );
    assert_eq!(
        origin_file(&e, "main", "moved.txt").as_deref(),
        Some("moved\n"),
        "both moves are on main"
    );
}
