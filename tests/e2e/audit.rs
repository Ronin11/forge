use crate::support::*;

fn audit_json(e: &Env, args: &[&str]) -> serde_json::Value {
    let mut a = vec!["audit", "--json"];
    a.extend_from_slice(args);
    let o = e.forge("ok.sh", &a);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

fn set(e: &Env, id: i64, state: &str, reason: &str) {
    e.db()
        .execute(
            "UPDATE tasks SET state=?2, reason=?3 WHERE id=?1",
            rusqlite::params![id, state, reason],
        )
        .unwrap();
}

#[test]
fn every_outcome_is_totaled_with_its_task_count_and_cost() {
    let e = Env::new();
    let landed = e.add(&[]);
    set(&e, landed, "succeeded", "");
    e.db()
        .execute(
            "UPDATE tasks SET landed_sha='abc123' WHERE id=?1",
            rusqlite::params![landed],
        )
        .unwrap();
    e.db()
        .execute(
            "INSERT INTO attempts (task_id, attempt_no, step, state, cost_usd, started_at) VALUES (?1, 1, 'code', 'succeeded', 1.5, 0)",
            rusqlite::params![landed],
        )
        .unwrap();

    let in_progress = e.add(&[]);

    let withdrawn = e.add(&[]);
    set(&e, withdrawn, "withdrawn", "stale description");

    let unverified = e.add(&[]);
    set(&e, unverified, "unverified", "no L1 or L2");

    let question = e.add(&[]);
    set(&e, question, "blocked", "needs input: which database?");

    let dangling = e.add(&[]);
    set(&e, dangling, "failed", "L1 failed: test");

    let doc = audit_json(&e, &[]);
    let outcomes = &doc["outcomes"];
    assert_eq!(outcomes["landed"]["tasks"], 1, "{doc}");
    assert_eq!(outcomes["landed"]["cost_usd"], 1.5, "{doc}");
    assert_eq!(outcomes["in_progress"]["tasks"], 1, "{doc}");
    assert_eq!(outcomes["withdrawn"]["tasks"], 1, "{doc}");
    assert_eq!(outcomes["unverified"]["tasks"], 1, "{doc}");
    assert_eq!(outcomes["question"]["tasks"], 1, "{doc}");
    assert_eq!(outcomes["dangling"]["tasks"], 1, "{doc}");

    let dangling_list = doc["dangling"].as_array().unwrap();
    assert_eq!(dangling_list.len(), 1, "{doc}");
    assert_eq!(dangling_list[0]["id"], dangling);
    assert_eq!(dangling_list[0]["reason"], "L1 failed: test");

    // The text form names the same tip.
    let o = e.forge("ok.sh", &["audit"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("dangling tips:"), "{out}");
    assert!(
        out.contains(&format!("task {dangling}: L1 failed: test")),
        "{out}"
    );
}

#[test]
fn a_retried_failure_is_in_progress_not_dangling_at_its_tip() {
    let e = Env::new();
    let root = e.add(&[]);
    set(&e, root, "failed", "L1 failed: test");
    let retry = e.add(&[]);
    e.db()
        .execute(
            "UPDATE tasks SET retry_of=?2 WHERE id=?1",
            rusqlite::params![retry, root],
        )
        .unwrap();

    let doc = audit_json(&e, &[]);
    assert_eq!(doc["dangling"].as_array().unwrap().len(), 0, "{doc}");
    assert_eq!(doc["outcomes"]["in_progress"]["tasks"], 2, "{doc}");
}

#[test]
fn a_blocked_workflow_request_is_dangling_not_a_question() {
    let e = Env::new();
    let id = e.add(&[]);
    set(&e, id, "blocked", "needs workflow: no e2e step");

    let doc = audit_json(&e, &[]);
    assert_eq!(doc["outcomes"]["question"]["tasks"], 0, "{doc}");
    assert_eq!(doc["outcomes"]["dangling"]["tasks"], 1, "{doc}");
    assert_eq!(doc["dangling"][0]["id"], id);
}

#[test]
fn since_excludes_a_lineage_with_no_activity_in_the_window() {
    let e = Env::new();
    let id = e.add(&[]);
    set(&e, id, "failed", "L1 failed: test");
    e.db()
        .execute(
            "UPDATE tasks SET created_at = created_at - 100000 WHERE id=?1",
            rusqlite::params![id],
        )
        .unwrap();

    let doc = audit_json(&e, &["--since", "1h"]);
    assert_eq!(doc["dangling"].as_array().unwrap().len(), 0, "{doc}");

    let doc = audit_json(&e, &["--since", "999d"]);
    assert_eq!(doc["dangling"].as_array().unwrap().len(), 1, "{doc}");
}

#[test]
fn doctor_warns_on_a_dangling_tip_and_names_it() {
    let e = Env::new();
    let id = e.add(&[]);
    set(&e, id, "failed", "L1 failed: test");

    let o = e.forge("ok.sh", &["doctor", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let checks: Vec<serde_json::Value> = serde_json::from_slice(&o.stdout).unwrap();
    let row = checks
        .iter()
        .find(|c| c["name"] == "dangling")
        .expect("a dangling row");
    assert_eq!(row["status"], "warn", "{row}");
    assert!(
        row["detail"]
            .as_str()
            .unwrap()
            .contains(&format!("task {id}")),
        "{row}"
    );
}

#[test]
fn doctor_is_ok_with_nothing_dangling() {
    let e = Env::new();
    e.add(&[]);

    let o = e.forge("ok.sh", &["doctor", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let checks: Vec<serde_json::Value> = serde_json::from_slice(&o.stdout).unwrap();
    let row = checks
        .iter()
        .find(|c| c["name"] == "dangling")
        .expect("a dangling row");
    assert_eq!(row["status"], "ok", "{row}");
}
