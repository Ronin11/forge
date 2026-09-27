//! Landing reads the base straight from the remote into the kernel
//! repository, not through the registered checkout's tracking ref.

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
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn landing_lands_on_the_remotes_tip_even_when_the_checkouts_tracking_ref_cannot_see_it() {
    // The registered checkout's `remote.origin.fetch` is narrowed to a
    // branch that is not the base, so a plain `git fetch origin main` in
    // it never moves `refs/remotes/origin/main`. Landing must not read the
    // base through that stale ref: it fetches straight from the URL.
    let e = Env::new();
    git(&e.repo, &["push", "-q", "origin", "main"]);
    git(&e.repo, &["fetch", "-q", "origin"]);
    git(&e.repo, &["config", "--unset-all", "remote.origin.fetch"]);
    git(
        &e.repo,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/heads/decoy:refs/remotes/origin/decoy",
        ],
    );
    let stale = git(&e.repo, &["rev-parse", "refs/remotes/origin/main"]);
    let other = e._dir.path().join("other");
    let o = Command::new("git")
        .args(["clone", "-q"])
        .arg(&e.repo)
        .arg(&other)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    git(&other, &["config", "user.name", "Other"]);
    git(&other, &["config", "user.email", "other@example.com"]);
    std::fs::write(other.join("moved.txt"), "moved\n").unwrap();
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "-qm", "main moved"]);
    git(
        &other,
        &["push", "-q", e.origin.to_str().unwrap(), "main:main"],
    );
    let moved = origin_sha(&e, "main");
    assert_ne!(moved, stale, "the remote's main moved past the checkout");
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(pushed);
    // Landed on the remote's actual tip, past the checkout's stale ref, in
    // a single round: no retry from a stale read to catch up on.
    assert_eq!(
        op_names(&e, 1),
        vec![
            ("clone".into(), true),
            ("setup".into(), true),
            ("repo-map".into(), true),
            ("verify".into(), true),
            ("integrate".into(), true),
            ("push".into(), true),
            ("land".into(), true)
        ]
    );
    assert_eq!(
        origin_file(&e, "main", "moved.txt").as_deref(),
        Some("moved\n"),
        "the merge carried the remote's moved base in"
    );
    assert_eq!(
        origin_file(&e, "main", "answer.txt").as_deref(),
        Some("42\n")
    );
    assert_eq!(
        git(&e.repo, &["rev-parse", "refs/remotes/origin/main"]),
        stale,
        "the checkout's own tracking ref is still stale; landing never read through it"
    );
}
