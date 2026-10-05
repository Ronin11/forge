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
    assert!(text.contains(&workflow), "{text}");
    assert!(text.contains("TRIPPED"), "{text}");
    assert!(text.contains("NEAR"), "{text}");
    for signal in ["no_edit_calls", "edits_without_commit", "repeats"] {
        assert!(text.contains(signal), "{text}");
    }
}

/// `forge add --early-ending` stores the given thresholds as JSON on the
/// task (any subset; the rest come from the operator's `[early_ending]`
/// config), shown by `forge show` and `forge show --json`; `forge task
/// set --early-ending` replaces them the same way `--max-turns` does.
#[test]
fn forge_add_early_ending_stores_thresholds_and_forge_task_set_replaces_them() {
    let e = Env::new();
    let id = e.add(&["--early-ending", "no_edit_calls=5,repeats=3"]);

    let o = e.forge("ok.sh", &["show", &id.to_string()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains(r#"early-ending {"no_edit_calls":5,"repeats":3}"#),
        "{out}"
    );

    let show_json = |id: i64| -> serde_json::Value {
        serde_json::from_slice(
            &e.forge("ok.sh", &["show", &id.to_string(), "--json"])
                .stdout,
        )
        .unwrap()
    };
    let t = show_json(id);
    assert_eq!(t["early_ending"], r#"{"no_edit_calls":5,"repeats":3}"#);

    let o = e.forge(
        "ok.sh",
        &[
            "task",
            "set",
            &id.to_string(),
            "--early-ending",
            "signals_to_end=1",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let t = show_json(id);
    assert_eq!(t["early_ending"], r#"{"signals_to_end":1}"#);

    // A task filed with no `--early-ending` stores none: the operator's
    // config applies.
    let unset = e.add(&[]);
    assert_eq!(show_json(unset)["early_ending"], serde_json::Value::Null);
}

/// An unknown `--early-ending` key, or a non-integer value, is refused by
/// name at argument-parsing time, before a task is ever filed; the same
/// on `forge task set`.
#[test]
fn forge_add_early_ending_rejects_an_unknown_key_or_a_non_integer_naming_it() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    // A task that exists before the bad calls below, so `e.db()` has a
    // database to open and the count check below has a baseline.
    let baseline = e.add(&[]);

    let o = e.forge(
        "ok.sh",
        &[
            "add",
            repo,
            "write 42 to answer.txt",
            "--early-ending",
            "bogus=5",
        ],
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("bogus"), "{err}");
    assert_eq!(
        e.db()
            .query_row("SELECT count(*) FROM tasks", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1,
        "no further task was filed"
    );

    let o = e.forge(
        "ok.sh",
        &[
            "add",
            repo,
            "write 42 to answer.txt",
            "--early-ending",
            "repeats=notanumber",
        ],
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("repeats"), "{err}");

    let o = e.forge(
        "ok.sh",
        &[
            "task",
            "set",
            &baseline.to_string(),
            "--early-ending",
            "bogus=5",
        ],
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("bogus"), "{err}");
}
