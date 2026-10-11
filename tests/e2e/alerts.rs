//! What reaches the operator unasked (docs/OPS.md): a failed doctor-daily
//! goes out through the channel plugins and stands on the portal page
//! until a run passes; a step that printed nothing names what it ran.
use crate::audience::{events, notify};
use crate::support::*;
use serde_json::Value;
use std::fs;

fn daily_project(e: &Env) {
    let repo = e.repo.to_str().unwrap();
    let out = e.forge(
        "ok.sh",
        &[
            "project",
            "new",
            "daily",
            "--purpose",
            "daily report",
            "--repo",
            repo,
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
}

/// doctor-daily in the operator's catalog: one step that writes a file,
/// then `assert`, which decides whether the run passed.
fn run_daily(e: &Env, assert: &str) -> i64 {
    fs::write(
        e.home.join("workflows/doctor-daily.toml"),
        format!(
            r#"
name = "doctor-daily"
kind = "run"
steps = [{{ action = "write-file", effect = "file" }}]
[trigger]
on = "manual"
[assert]
clean = {assert}
[limits]
budget_usd = 1.0
per_day = 10
on_failure = "drop"
"#
        ),
    )
    .unwrap();
    let input = e._dir.path().join("daily-input.json");
    fs::write(&input, r#"{"path":"out.txt","text":"doctor complete"}"#).unwrap();
    let out = e.forge(
        "ok.sh",
        &[
            "job",
            "start",
            "daily",
            "doctor-daily",
            "--input",
            input.to_str().unwrap(),
            "--now",
        ],
    );
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .lines()
        .last()
        .and_then(|l| l.trim().parse().ok())
        .unwrap_or_else(|| panic!("no job id: {}", String::from_utf8_lossy(&out.stderr)))
}

fn alerts(e: &Env, kind: &str) -> Vec<Value> {
    events(e)
        .into_iter()
        .filter(|v| v["type"] == "operator_alert" && v["kind"] == kind)
        .collect()
}

fn portal_alerts(e: &Env) -> Vec<String> {
    let out = e.forge("ok.sh", &["project", "view", "daily", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    doc["alerts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_failing_doctor_daily_notifies_and_stands_on_the_portal_until_a_run_passes() {
    let e = Env::new();
    daily_project(&e);
    let failed = run_daily(
        &e,
        r#"["sh", "-c", "echo 'FAIL: forge doctor exited 101'; exit 1"]"#,
    );

    let sent = alerts(&e, "doctor_failed");
    assert_eq!(sent.len(), 1, "{sent:?}");
    let text = sent[0]["text"].as_str().unwrap();
    assert!(
        text.contains(&format!("job {failed}")) && text.contains("forge doctor exited 101"),
        "{text}"
    );
    assert_eq!(sent[0]["audience"], "person");

    // Through the notify plugin, the way the operator hears it.
    let hits = notify(&e, &events(&e), "", &e._dir.path().join("notify"));
    assert!(
        hits.iter()
            .any(|h| h.starts_with("alert|doctor_failed|") && h.contains("forge doctor exited 101")),
        "{hits:?}"
    );

    // On the message record, which is what says it was sent.
    let out = e.forge("ok.sh", &["message", "list", "daily", "--json"]);
    let msgs: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        msgs.as_array()
            .unwrap()
            .iter()
            .any(|m| m["channel"] == "alerts"
                && m["text"].as_str().unwrap().contains("doctor-daily failed")),
        "{msgs:?}"
    );

    let standing = portal_alerts(&e);
    assert_eq!(standing.len(), 1, "{standing:?}");
    assert!(standing[0].contains("doctor-daily failed"), "{standing:?}");

    run_daily(&e, r#"["true"]"#);
    assert!(
        portal_alerts(&e).is_empty(),
        "a passing run clears the line"
    );
    assert_eq!(alerts(&e, "doctor_failed").len(), 1, "a pass sends nothing");
}

#[test]
fn a_step_that_prints_nothing_is_escalated_naming_what_it_ran_and_where() {
    let e = Env::new();
    daily_project(&e);
    run_daily(&e, r#"["sh", "-c", "exit 3"]"#);
    let silent = alerts(&e, "tool_silent");
    assert_eq!(silent.len(), 1, "{silent:?}");
    let text = silent[0]["text"].as_str().unwrap();
    assert!(text.contains("the tool produced nothing"), "{text}");
    assert!(
        text.contains("exited code 3") && text.contains("it ran `sh` in /"),
        "{text}"
    );
}
