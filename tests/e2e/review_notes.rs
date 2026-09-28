//! A review demotion carries a reproduction a fresh clone can run: a
//! demotion citing the reviewer's sandbox is refused and the reviewer is
//! asked once to inline it; notes written under tests/review-notes/<task>/
//! travel with the demotion to its follow-up and to `forge show`.

use crate::support::*;

#[test]
fn a_reviewer_citing_tmp_is_asked_once_and_its_inline_demotion_stands() {
    let e = Env::new();
    let o = run_wf(
        &e,
        "ok.sh",
        &[("FORGE_CLAUDE_BIN_REVIEW", "reviewer-tmp-then-inline.sh")],
        "reviewed",
        "write 42",
    );
    assert!(!o.status.success());
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "blocked");
    assert!(
        reason.starts_with("review demoted: answer.txt has no trailing newline"),
        "{reason}"
    );
    assert!(!reason.contains("/tmp"), "{reason}");
    // code, the refused review, the inlined review; the refusal was not
    // counted against --retries 0.
    let a = e.attempts(1);
    assert_eq!(a.len(), 3, "{a:?}");
    assert_eq!(a[1].1, "checks_failed");
    assert_eq!(
        check(&a[1].4, "L0", "reproduction-self-contained"),
        Some(false)
    );
    assert_eq!(
        check(&a[2].4, "L0", "reproduction-self-contained"),
        Some(true)
    );
    let refunded: i64 = e
        .db()
        .query_row(
            "SELECT refunded FROM attempts WHERE task_id=1 AND attempt_no=?1",
            [a[1].0],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(refunded, 1);
    // The follow-up is given the inline reproduction, never the sandbox path.
    let doc = e.trace_json("2");
    let text = doc["task"]["text"].as_str().unwrap();
    assert!(text.contains("`tail -c 1 answer.txt | od -c`"), "{text}");
    assert!(!text.contains("/tmp"), "{text}");
    let show = String::from_utf8_lossy(&e.forge("ok.sh", &["show", "1"]).stdout).into_owned();
    assert!(show.contains("reproduction:"), "{show}");
    assert!(show.contains("`tail -c 1 answer.txt | od -c`"), "{show}");
}

#[test]
fn a_reviewer_that_keeps_citing_tmp_does_not_demote() {
    let e = Env::new();
    let o = run_wf(
        &e,
        "ok.sh",
        &[("FORGE_CLAUDE_BIN_REVIEW", "reviewer-tmp.sh")],
        "reviewed",
        "write 42",
    );
    assert!(!o.status.success());
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "failed", "{reason}");
    assert!(reason.contains("reproduction-self-contained"), "{reason}");
    let n: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "no follow-up is filed");
}

#[test]
fn review_notes_leave_the_branch_and_travel_with_the_demotion() {
    let e = Env::new();
    let o = run_wf(
        &e,
        "ok.sh",
        &[("FORGE_CLAUDE_BIN_REVIEW", "reviewer-notes.sh")],
        "reviewed",
        "write 42",
    );
    assert!(!o.status.success());
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "blocked", "{reason}");
    let a = e.attempts(1);
    assert_eq!(a.len(), 2, "{a:?}");
    assert_eq!(check(&a[1].4, "L0", "no-writes"), Some(true));
    assert_eq!(check(&a[1].4, "L0", "clean-tree"), Some(true));
    assert_eq!(
        check(&a[1].4, "L0", "reproduction-self-contained"),
        Some(true)
    );
    let branch: String = e
        .db()
        .query_row("SELECT branch FROM tasks WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        origin_file(&e, &branch, "tests/review-notes/1/repro.sh"),
        None
    );
    let envelope: String = e
        .db()
        .query_row(
            "SELECT envelope_json FROM attempts WHERE task_id=1 AND attempt_no=?1",
            [a[1].0],
            |r| r.get(0),
        )
        .unwrap();
    let env: serde_json::Value = serde_json::from_str(&envelope).unwrap();
    assert_eq!(
        env["review_notes"][0]["path"],
        "tests/review-notes/1/repro.sh"
    );
    let doc = e.trace_json("2");
    let text = doc["task"]["text"].as_str().unwrap();
    assert!(
        text.contains("# forge review repro\ntail -c 1 answer.txt | od -c"),
        "{text}"
    );
    let show = String::from_utf8_lossy(&e.forge("ok.sh", &["show", "1"]).stdout).into_owned();
    assert!(
        show.contains("Reproduction file tests/review-notes/1/repro.sh"),
        "{show}"
    );
    assert!(show.contains("# forge review repro"), "{show}");
}
