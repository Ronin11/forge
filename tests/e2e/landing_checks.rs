use crate::landing::origin_sha;
use crate::support::*;

// This check passes in the code worktree and fails on a merged tree that
// carries the base's later commit, so it trips only when landing verifies
// again after the base moved; the unchanged case is proven by the integrate
// op's own detail. A manual landing makes both base cases deterministic,
// without racing a coder. (A detector keyed on the worktree's directory
// name fired on the code attempt when Forge landed this change itself.)
fn landing_with_a_check_that_rejects_reruns(base_moves: bool) {
    let e = Env::new();
    std::fs::write(
        e.repo.join("landing-check.sh"),
        "[ ! -f new-base.txt ] || { echo 'landing reran the check'; exit 1; }\n",
    )
    .unwrap();
    let config = std::fs::read_to_string(e.repo.join("forge.toml")).unwrap();
    std::fs::write(
        e.repo.join("forge.toml"),
        format!("{config}rerun = [\"bash\", \"landing-check.sh\"]\n"),
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "detect landing checks"]);
    git(&e.repo, &["push", "-q", "origin", "main"]);
    let o = e.run("ok.sh", &["--retries", "0"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let verified: String = e
        .db()
        .query_row(
            "SELECT end_sha FROM attempts WHERE task_id=1 AND state='succeeded' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    if base_moves {
        std::fs::write(e.repo.join("new-base.txt"), "base moved\n").unwrap();
        git(&e.repo, &["add", "new-base.txt"]);
        git(&e.repo, &["commit", "-qm", "advance main"]);
        git(&e.repo, &["push", "-q", "origin", "main"]);
    }
    let base = origin_sha(&e, "main");
    let o = e.forge("ok.sh", &["land", "1"]);
    let (ok, detail): (bool, String) = e
        .db()
        .query_row(
            "SELECT ok, detail FROM ops WHERE task_id=1 AND name='integrate' ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    if base_moves {
        assert!(!o.status.success());
        assert!(!ok, "{detail}");
        assert!(detail.contains("merged main @"), "{detail}");
        assert!(detail.contains("L1 failed: rerun"), "{detail}");
        assert_eq!(origin_sha(&e, "main"), base);
    } else {
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert!(ok, "{detail}");
        assert_eq!(
            detail,
            format!("base unchanged since verify at {verified}; checks not re-run")
        );
        assert_eq!(origin_sha(&e, "main"), verified);
    }
}

#[test]
fn landing_skips_checks_for_the_verified_commit_on_an_unchanged_base() {
    landing_with_a_check_that_rejects_reruns(false);
}

#[test]
fn landing_reruns_checks_when_the_base_moves() {
    landing_with_a_check_that_rejects_reruns(true);
}
