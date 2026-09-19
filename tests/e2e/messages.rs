use crate::support::*;

fn new_project(e: &Env, name: &str) {
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", name, "--purpose", "p", "--repo", repo],
        )
        .status
        .success()
    );
}

/// Force a message's `at` column to a controlled value: recording three
/// messages back to back can otherwise land in the same unix second,
/// which would make a `--since` test flaky rather than a real check.
fn set_at(e: &Env, contact: &str, text: &str, at: i64) {
    e.db()
        .execute(
            "UPDATE messages SET at = ?1 WHERE contact = ?2 AND text = ?3",
            rusqlite::params![at, contact, text],
        )
        .unwrap();
}

#[test]
fn record_then_list_round_trips_and_since_filters() {
    let e = Env::new();
    new_project(&e, "demo");

    let o = e.forge(
        "ok.sh",
        &[
            "message",
            "record",
            "demo",
            "--channel",
            "signal",
            "--from",
            "alice",
            "--text",
            "hi there",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    set_at(&e, "alice", "hi there", 1000);

    // A message with neither --from nor --to is refused.
    let bad = e.forge(
        "ok.sh",
        &[
            "message",
            "record",
            "demo",
            "--channel",
            "signal",
            "--text",
            "x",
        ],
    );
    assert!(!bad.status.success());

    // --from and --to together are refused.
    let bad = e.forge(
        "ok.sh",
        &[
            "message",
            "record",
            "demo",
            "--channel",
            "signal",
            "--from",
            "alice",
            "--to",
            "bob",
            "--text",
            "x",
        ],
    );
    assert!(!bad.status.success());

    // An unknown project is refused.
    let bad = e.forge(
        "ok.sh",
        &[
            "message",
            "record",
            "nope",
            "--channel",
            "signal",
            "--from",
            "alice",
            "--text",
            "x",
        ],
    );
    assert!(!bad.status.success());

    let o = e.forge(
        "ok.sh",
        &[
            "message",
            "record",
            "demo",
            "--channel",
            "signal",
            "--to",
            "alice",
            "--text",
            "on it",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    set_at(&e, "alice", "on it", 2000);

    // A task-scoped message.
    let task_id = e.add(&[]);
    let o = e.forge(
        "ok.sh",
        &[
            "message",
            "record",
            "demo",
            "--channel",
            "signal",
            "--from",
            "bob",
            "--text",
            "what's up with my task",
            "--task",
            &task_id.to_string(),
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    set_at(&e, "bob", "what's up with my task", 3000);

    // A nonexistent task is refused.
    let bad = e.forge(
        "ok.sh",
        &[
            "message",
            "record",
            "demo",
            "--channel",
            "signal",
            "--from",
            "carol",
            "--text",
            "x",
            "--task",
            "999999",
        ],
    );
    assert!(!bad.status.success());

    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["message", "list", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 3, "{rows:?}");
    // Newest first (by id, which agrees with the `at` values set above).
    assert_eq!(rows[0]["contact"], "bob");
    assert_eq!(rows[0]["direction"], "in");
    assert_eq!(rows[0]["text"], "what's up with my task");
    assert_eq!(rows[0]["task_id"], task_id);
    assert_eq!(rows[1]["contact"], "alice");
    assert_eq!(rows[1]["direction"], "out");
    assert_eq!(rows[1]["text"], "on it");
    assert!(rows[1]["task_id"].is_null());
    assert_eq!(rows[2]["contact"], "alice");
    assert_eq!(rows[2]["direction"], "in");
    assert_eq!(rows[2]["text"], "hi there");
    assert_eq!(rows[2]["channel"], "signal");
    assert_eq!(rows[2]["project"], "demo");

    // --contact narrows to one contact's messages.
    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge(
            "ok.sh",
            &["message", "list", "demo", "--contact", "bob", "--json"],
        )
        .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["contact"], "bob");

    // --direction narrows to one direction.
    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge(
            "ok.sh",
            &["message", "list", "demo", "--direction", "out", "--json"],
        )
        .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["direction"], "out");

    // --since excludes messages recorded strictly before it.
    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge(
            "ok.sh",
            &["message", "list", "demo", "--since", "2000", "--json"],
        )
        .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["text"], "what's up with my task");
    assert_eq!(rows[1]["text"], "on it");

    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge(
            "ok.sh",
            &["message", "list", "demo", "--since", "2500", "--json"],
        )
        .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["text"], "what's up with my task");

    // An unknown direction is refused.
    let bad = e.forge(
        "ok.sh",
        &[
            "message",
            "list",
            "demo",
            "--direction",
            "sideways",
            "--json",
        ],
    );
    assert!(!bad.status.success());
}
