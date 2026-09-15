use crate::support::*;

/// Parse `created initiative N` from `forge initiative new`'s stdout.
fn created_id(o: &std::process::Output) -> i64 {
    let out = String::from_utf8_lossy(&o.stdout);
    out.lines()
        .find_map(|l| l.strip_prefix("created initiative "))
        .unwrap_or_else(|| panic!("{out}"))
        .parse()
        .unwrap()
}

#[test]
fn an_initiative_from_a_file_lands_a_dependency_and_settles_done_with_the_event() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo],
        )
        .status
        .success()
    );

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("tasks.txt");
    std::fs::write(
        &file,
        "write 42 to answer.txt\n\nafter: 1\nadd an extra file",
    )
    .unwrap();

    let o = e.forge(
        "ok.sh",
        &[
            "initiative",
            "new",
            "demo",
            "--outcome",
            "answer.txt says 42 everywhere it matters",
            "--from",
            file.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = created_id(&o);

    // Both tasks landed in the initiative, the second waiting on the first.
    let after: String = e
        .db()
        .query_row("SELECT after_json FROM tasks WHERE id=2", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, "[1]");
    let initiatives: (Option<i64>, Option<i64>) = e
        .db()
        .query_row(
            "SELECT (SELECT initiative FROM tasks WHERE id=1), (SELECT initiative FROM tasks WHERE id=2)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(initiatives, (Some(id), Some(id)));

    // `ok.sh` lands the first task, then `addfile.sh` gives the second a
    // real change of its own (the base already has `answer.txt`, so
    // `ok.sh` again would report a change git does not see).
    assert!(
        e.forge("ok.sh", &["work", "--once", "--max-tasks", "1"])
            .status
            .success()
    );
    assert_eq!(e.task(1).0, "succeeded");
    assert!(e.forge("addfile.sh", &["work", "--once"]).status.success());
    assert_eq!(e.task(2).0, "succeeded");

    // The initiative settled once the last task reached a terminal state.
    let events = std::fs::read_to_string(e.home.join("events.jsonl")).unwrap();
    let settled = events
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["type"] == "initiative_settled")
        .unwrap_or_else(|| panic!("no initiative_settled event in:\n{events}"));
    assert_eq!(settled["id"], id);
    assert_eq!(settled["state"], "done");

    let (settled_at,): (Option<i64>,) = e
        .db()
        .query_row(
            "SELECT settled_at FROM initiatives WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?,)),
        )
        .unwrap();
    assert!(settled_at.is_some());

    let report = e.forge(
        "ok.sh",
        &["initiative", "report", &id.to_string(), "--json"],
    );
    let doc: serde_json::Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(doc["state"], "done");
    assert_eq!(doc["outcome"], "answer.txt says 42 everywhere it matters");
    assert_eq!(doc["tasks"].as_array().unwrap().len(), 2);
}

#[test]
fn same_rule_failures_hold_the_initiative_and_the_report_names_the_rule() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo],
        )
        .status
        .success()
    );

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("tasks.txt");
    // `noop.sh` commits nothing, so every one of these fails L0's
    // has-commits rule, deterministically and independently.
    std::fs::write(&file, "first task\n\nsecond task\n\nthird task").unwrap();

    let o = e.forge(
        "noop.sh",
        &[
            "initiative",
            "new",
            "demo",
            "--outcome",
            "three independent things get done",
            "--from",
            file.to_str().unwrap(),
            "--stop-after",
            "2",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = created_id(&o);

    assert!(e.forge("noop.sh", &["work", "--once"]).status.success());

    assert_eq!(e.task(1).0, "failed");
    assert_eq!(e.task(2).0, "failed");
    assert!(
        e.task(1).1.starts_with("L0 failed: has-commits"),
        "{}",
        e.task(1).1
    );
    assert!(
        e.task(2).1.starts_with("L0 failed: has-commits"),
        "{}",
        e.task(2).1
    );
    // The third task never got a chance: the worker stopped claiming this
    // initiative's tasks after two same-rule failures in a row.
    assert_eq!(e.task(3).0, "queued");

    let o = e.forge(
        "noop.sh",
        &["initiative", "show", &id.to_string(), "--json"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let row: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(row["state"], "held");
    assert_eq!(row["held_rule"], "has-commits");

    let o = e.forge(
        "noop.sh",
        &["initiative", "report", &id.to_string(), "--json"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(doc["state"], "held");
    assert_eq!(doc["held_rule"], "has-commits");
    let refused = doc["refused"].as_array().unwrap();
    assert!(
        refused
            .iter()
            .any(|r| r["rule"] == "has-commits" && r["count"].as_i64().unwrap() >= 2),
        "{refused:?}"
    );

    // The text form names the rule too.
    let o = e.forge("noop.sh", &["initiative", "report", &id.to_string()]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("held"), "{out}");
    assert!(out.contains("has-commits"), "{out}");
}

#[test]
fn a_task_blocked_on_a_question_keeps_the_initiative_open() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo],
        )
        .status
        .success()
    );

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("tasks.txt");
    std::fs::write(&file, "write an answer").unwrap();

    let o = e.forge(
        "commitneedsinput.sh",
        &[
            "initiative",
            "new",
            "demo",
            "--outcome",
            "an answer is on record",
            "--from",
            file.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = created_id(&o);

    // The task commits an answer, then asks the operator a question
    // instead of finishing: not terminal, not landed, not withdrawn.
    assert!(
        e.forge("commitneedsinput.sh", &["work", "--once"])
            .status
            .success()
    );
    assert_eq!(e.task(1).0, "blocked");

    let o = e.forge(
        "commitneedsinput.sh",
        &["initiative", "show", &id.to_string(), "--json"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let row: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(row["state"], "open");
    assert_eq!(row["blocked"], 1);
    assert_eq!(row["settled_at"], serde_json::Value::Null);

    // No settlement event fired for a still-blocked initiative.
    let events_path = e.home.join("events.jsonl");
    if events_path.exists() {
        let events = std::fs::read_to_string(&events_path).unwrap();
        assert!(
            !events
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .any(|v| v["type"] == "initiative_settled"),
            "{events}"
        );
    }
}
