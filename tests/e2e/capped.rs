use crate::support::*;

#[test]
fn a_task_whose_second_attempt_would_cross_the_cap_ends_capped_with_its_branch_and_handoff() {
    let e = Env::new();
    let o = e.run("wrongsession.sh", &["--budget", "0.015", "--retries", "3"]);
    assert!(!o.status.success());
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "capped", "{reason}");
    assert!(
        reason.starts_with("$0.01 of $0.01"),
        "the reason names spent and cap: {reason}"
    );
    assert!(pushed, "the branch stays pushed");
    assert!(
        e.origin_branches().contains("forge/1-"),
        "{}",
        e.origin_branches()
    );
    assert_eq!(
        e.attempts(1).len(),
        1,
        "the second attempt was never started: the cap is not overshot"
    );
    let (session, handoff): (String, String) = e
        .db()
        .query_row(
            "SELECT session_id, handoff FROM tasks WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(session, "sess-wrong-1");
    assert!(handoff.contains("Handoff:"), "{handoff}");
    assert!(handoff.contains("write 42 to answer.txt"), "{handoff}");
}

#[test]
fn a_capped_task_is_listed_and_counted_apart_from_failed() {
    let e = Env::new();
    let o = e.run("wrongsession.sh", &["--budget", "0.015", "--retries", "3"]);
    assert!(!o.status.success());
    let o = e.forge("ok.sh", &["log", "--state", "capped", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
    assert_eq!(rows[0]["state"], "capped");
    let o = e.forge("ok.sh", &["log", "--state", "failed", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(rows.as_array().unwrap().is_empty(), "{rows}");
    let o = e.forge("ok.sh", &["stats", "--json"]);
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let w = &doc["workflows"][0];
    assert_eq!(w["capped"], 1, "{w}");
    assert_eq!(w["failed"], 0, "{w}");
}
