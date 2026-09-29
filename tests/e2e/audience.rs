//! Real recovery events delivered through the reference plugins.
use crate::support::*;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

fn executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn events(e: &Env) -> Vec<Value> {
    let out = e.forge("ok.sh", &["events", "--since", "0:0"]);
    assert!(out.status.success());
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Replay the real CLI event stream through a finite transport, so there
/// is no subscription race or arbitrary sleep before a task finishes.
fn notify(e: &Env, rows: &[Value], config: &str, state: &Path) -> Vec<String> {
    fs::create_dir_all(state).unwrap();
    fs::write(state.join("config"), config).unwrap();
    fs::write(state.join("cursor"), "0:0").unwrap();
    fs::write(
        state.join("events"),
        rows.iter().map(|v| format!("{v}\n")).collect::<String>(),
    )
    .unwrap();
    executable(
        &state.join("forge"),
        "#!/bin/sh\ncat \"$FORGE_PLUGIN_STATE/events\"\n",
    );
    fs::write(state.join("command"), "cat >/dev/null\nprintf '%s|%s|%s\\n' \"$1\" \"$2\" \"$3\" >>\"$FORGE_PLUGIN_STATE/hits\"\n").unwrap();
    let out = Command::new("sh")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/plugins/notify/notify.sh"
        ))
        .env("FORGE_BIN", state.join("forge"))
        .env("FORGE_HOME", &e.home)
        .env("FORGE_PLUGIN_DIR", state)
        .env("FORGE_PLUGIN_STATE", state)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    fs::read_to_string(state.join("hits"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_demotion_followup_is_quiet_but_an_operator_question_runs_the_plugin_command() {
    let e = Env::new();
    run_wf(
        &e,
        "ok.sh",
        &[("FORGE_CLAUDE_BIN_REVIEW", "reviewer-repro.sh")],
        "reviewed",
        "write 42",
    );
    assert_eq!(e.task(1).0, "blocked");
    assert_eq!(e.task(2).0, "queued");
    run_wf(
        &e,
        "needsinput.sh",
        &[],
        "direct",
        "choose the answer filename",
    );
    let rows = events(&e);
    let done: Vec<_> = rows.iter().filter(|v| v["type"] == "task_done").collect();
    assert_eq!(done.len(), 2, "each transition is emitted once: {done:?}");
    assert_eq!(done[0]["audience"], "none");
    assert_eq!(done[1]["audience"], "person");
    let hits = notify(&e, &rows, "", &e._dir.path().join("notify"));
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].starts_with("3|blocked|needs input:"), "{hits:?}");
    let firehose = notify(
        &e,
        &rows,
        "NOTIFY_ON=blocked failed\n",
        &e._dir.path().join("firehose"),
    );
    assert_eq!(firehose.len(), 2, "explicit NOTIFY_ON retains the firehose");
}

fn doctor_daily(e: &Env) {
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "daily",
                "--purpose",
                "daily report",
                "--repo",
                e.repo.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    fs::write(
        e.home.join("workflows/doctor-daily.toml"),
        r#"
name = "doctor-daily"
kind = "run"
steps = [{ action = "write-file", effect = "file" }]
[trigger]
on = "manual"
[limits]
budget_usd = 1.0
per_day = 10
on_failure = "drop"
"#,
    )
    .unwrap();
    let input = e._dir.path().join("daily-input.json");
    fs::write(&input, r#"{"path":"out.txt","text":"doctor complete"}"#).unwrap();
    for _ in 0..2 {
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
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn doctor_daily_announces_yesterdays_followup_once_across_runs_and_plugin_restarts() {
    let e = Env::new();
    run_wf(
        &e,
        "ok.sh",
        &[("FORGE_CLAUDE_BIN_REVIEW", "reviewer-repro.sh")],
        "reviewed",
        "write 42",
    );
    e.db().execute("UPDATE decisions SET created_at = CAST(strftime('%s','now','start of day','-1 day') AS INTEGER) WHERE kind='demotion-as-task'", []).unwrap();
    doctor_daily(&e);
    let rows = events(&e);
    let digests: Vec<_> = rows
        .iter()
        .filter(|v| v["type"] == "notification_digest")
        .collect();
    assert_eq!(digests.len(), 2);
    assert!(
        digests[0]["text"]
            .as_str()
            .unwrap()
            .contains("1 demotions followed up")
    );
    let state = e._dir.path().join("notify-daily");
    let first = notify(&e, &rows, "", &state);
    assert_eq!(
        first.len(),
        1,
        "only the digest invokes a command: {first:?}"
    );
    assert!(first[0].starts_with("digest|"));
    assert_eq!(
        notify(&e, &rows, "", &state),
        first,
        "replay after restart adds no second digest"
    );
}

#[test]
fn signal_obeys_audience_and_sends_one_daily_line() {
    let e = Env::new();
    let state = e._dir.path().join("signal-audience");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("cursor"), "0:0").unwrap();
    fs::write(
        state.join("config"),
        "SIGNAL_ACCOUNT=+1\nSIGNAL_TO=+2\nPOLL_SECONDS=1\n",
    )
    .unwrap();
    let rows = [
        serde_json::json!({"type":"task_done","task":1,"state":"blocked","audience":"none","reason":"review demoted"}),
        serde_json::json!({"type":"task_done","task":2,"state":"blocked","audience":"person","reason":"needs input: choose"}),
        serde_json::json!({"type":"notification_digest","day":86400,"text":"yesterday: 1 demotions followed up"}),
        serde_json::json!({"type":"notification_digest","day":86400,"text":"yesterday: 1 demotions followed up"}),
    ];
    fs::write(
        state.join("events"),
        rows.iter().map(|v| format!("{v}\n")).collect::<String>(),
    )
    .unwrap();
    executable(
        &state.join("forge"),
        "#!/bin/sh\ncase $1 in events) cat \"$FORGE_PLUGIN_STATE/events\";; esac\n",
    );
    executable(
        &state.join("signal-cli"),
        "#!/bin/sh\nif [ \"$3\" = send ]; then printf '%s\\n' \"$5\" >>\"$FORGE_PLUGIN_STATE/hits\"; fi\n",
    );
    let mut cmd = Command::new("sh");
    cmd.arg(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/plugins/signal/signal.sh"
    ))
    .env("FORGE_BIN", state.join("forge"))
    .env("SIGNAL_CLI", state.join("signal-cli"))
    .env("FORGE_PLUGIN_DIR", &state)
    .env("FORGE_PLUGIN_STATE", &state)
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let mut plugin = Worker::spawn(&mut cmd);
    assert!(wait_until(
        || state.join("digest-day").exists(),
        Duration::from_secs(10)
    ));
    plugin.stop();
    let hits = fs::read_to_string(state.join("hits")).unwrap();
    assert_eq!(hits.lines().count(), 2, "{hits}");
    assert!(hits.contains("task 2 blocked: needs input: choose"));
    assert!(hits.contains("yesterday: 1 demotions followed up"));
}
