//! The deterministic known-fixes step: when a code attempt fails only
//! checks named in `[checks.fixable]`, the engine runs their fix commands,
//! commits as Forge, and re-runs the checks once before any second agent
//! attempt.

use crate::support::*;

#[test]
fn an_attempt_failing_only_a_fixable_check_is_fixed_committed_and_passes() {
    let e = Env::new();
    fixable_repo(&e);
    let o = e.run("ok.sh", &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(pushed);

    let attempts = e.attempts(1);
    assert_eq!(
        attempts.len(),
        1,
        "the fix must resolve the attempt without a second agent attempt: {attempts:?}"
    );
    assert_eq!(attempts[0].1, "succeeded");
    for (level, name) in [("L1", "answer"), ("L1", "fmt")] {
        assert_eq!(
            check(&attempts[0].4, level, name),
            Some(true),
            "{level} {name} in {}",
            attempts[0].4
        );
    }

    let ops = op_names(&e, 1);
    assert!(
        ops.iter().any(|(n, ok)| n == "known-fix" && *ok),
        "expected an ok `known-fix` operation row: {ops:?}"
    );
    assert!(ops.iter().any(|(n, ok)| n == "verify" && *ok), "{ops:?}");

    let doc = e.trace_json("1");
    let fix_op = doc["ops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "known-fix")
        .expect("a known-fix op row");
    assert!(fix_op["kernel"].as_bool().unwrap());
    let detail = fix_op["detail"].as_str().unwrap();
    assert!(detail.contains("fmt"), "{detail}");
    assert!(
        detail.contains("file") || detail.contains("insertion") || detail.contains("deletion"),
        "the fix op's detail should carry a diff stat: {detail}"
    );

    let branch: String = e
        .db()
        .query_row("SELECT branch FROM tasks WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let log = std::process::Command::new("git")
        .args([
            "--git-dir",
            e.origin.to_str().unwrap(),
            "log",
            "--format=%an <%ae> %s",
            &branch,
        ])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log.stdout);
    assert!(
        log.lines()
            .any(|l| l.starts_with("Forge <forge@localhost>") && l.contains("fmt")),
        "expected a Forge-authored commit naming the fixed check: {log}"
    );

    assert_eq!(
        origin_file(&e, &branch, "fmt.txt").as_deref(),
        Some("GOOD\n"),
        "the fake formatter's fix must have landed on the pushed branch"
    );
}

/// The fix's own commit is the new candidate: `l1_l2` re-runs after it and
/// must judge that commit, not reject it as a check that moved the tree.
/// Landing re-verifies the merged tree the same way (`verify_integration`),
/// so this also proves `candidate-unchanged` does not block the one
/// mutator it is meant to allow.
#[test]
fn a_known_fix_still_lands() {
    let e = Env::new();
    fixable_repo(&e);
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
    assert!(reason.starts_with("landed main @ "), "{reason}");
    assert!(pushed);

    let attempts = e.attempts(1);
    assert_eq!(attempts.len(), 1, "the fix resolves it without a retry");
    assert_eq!(
        check(&attempts[0].4, "L0", "candidate-unchanged"),
        Some(true),
        "the fix's own commit is the candidate the re-run checks judged"
    );
    let ops = op_names(&e, 1);
    assert!(ops.iter().any(|(n, ok)| n == "known-fix" && *ok), "{ops:?}");
    assert!(ops.iter().any(|(n, ok)| n == "land" && *ok), "{ops:?}");

    assert_eq!(
        origin_file(&e, "main", "fmt.txt").as_deref(),
        Some("GOOD\n"),
        "the fix landed on main"
    );
}
