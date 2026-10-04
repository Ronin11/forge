use crate::support::*;

#[test]
fn stats_early_json_counts_each_signal_per_workflow() {
    let e = Env::new();
    let o = e.run("ok.sh", &[]);
    assert!(o.status.success());
    let db = e.db();
    let workflow: String = db
        .query_row("SELECT workflow FROM tasks WHERE id=1", [], |r| r.get(0))
        .unwrap();
    db.execute(
        "UPDATE attempts SET early_signals='[\"no-edit\",\"repeat\"]',
                             early_near='[\"uncommitted\"]'
         WHERE task_id=1",
        [],
    )
    .unwrap();
    let attempts: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM attempts WHERE task_id=1 AND state != 'running'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(attempts > 0);

    let o = e.forge("ok.sh", &["stats", "--early", "--json"]);
    assert!(o.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let rows = doc.as_array().expect("an array of workflows");
    assert_eq!(rows.len(), 1, "{doc}");
    let w = &rows[0];
    assert_eq!(w["workflow"], workflow.as_str(), "{w}");
    assert_eq!(w["attempts"], attempts, "{w}");
    assert_eq!(
        w["early_signals"],
        serde_json::json!({"no_edit_calls": attempts, "edits_without_commit": 0, "repeats": attempts}),
        "{w}"
    );
    assert_eq!(
        w["early_near"],
        serde_json::json!({"no_edit_calls": 0, "edits_without_commit": attempts, "repeats": 0}),
        "{w}"
    );

    let o = e.forge("ok.sh", &["stats", "--early"]);
    assert!(o.status.success());
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(text.contains("edits_without_commit"), "{text}");
}
