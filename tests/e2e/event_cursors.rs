//! Cursor protocol regressions across rotation and consumer restart.
use crate::support::*;
use serde_json::Value;
use std::fs;

fn events(e: &Env, cursor: &str) -> Vec<Value> {
    let out = e.forge("ok.sh", &["events", "--since", cursor]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn event_cursors_resume_old_tail_then_new_file_without_fragments_or_duplicates() {
    let e = Env::new();
    fs::create_dir_all(&e.home).unwrap();
    let path = e.home.join("events.jsonl");
    let first = "{\"type\":\"note\",\"text\":\"seen\"}\n";
    fs::write(&path, first).unwrap();
    let snapshot = e.forge("ok.sh", &["snapshot"]);
    assert!(snapshot.status.success());
    let snapshot: Value = serde_json::from_slice(&snapshot.stdout).unwrap();
    let cursor = snapshot["events_offset"].as_str().unwrap();
    assert_eq!(cursor, format!("0:{}", first.len()));
    fs::write(
        &path,
        format!("{first}{{\"type\":\"note\",\"text\":\"old tail λ\"}}\n"),
    )
    .unwrap();
    fs::rename(&path, e.home.join("events.jsonl.1")).unwrap();
    fs::write(
        &path,
        "{\"generation\":1}\n{\"type\":\"note\",\"text\":\"new\"}\n",
    )
    .unwrap();
    let rows = events(&e, cursor);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["text"], "old tail λ");
    assert_eq!(rows[1]["text"], "new");
    assert!(rows[0]["cursor"].as_str().unwrap().starts_with("0:"));
    assert!(rows[1]["cursor"].as_str().unwrap().starts_with("1:"));
    assert_eq!(events(&e, rows[0]["cursor"].as_str().unwrap()), rows[1..]);
    assert!(events(&e, rows[1]["cursor"].as_str().unwrap()).is_empty());
}

#[test]
fn event_cursors_report_resync_when_history_is_unavailable() {
    let e = Env::new();
    fs::create_dir_all(&e.home).unwrap();
    let path = e.home.join("events.jsonl");
    fs::write(
        &path,
        "{\"generation\":3}\n{\"type\":\"note\",\"text\":\"retained\"}\n",
    )
    .unwrap();
    for cursor in ["1:0", "2:0", "9:0", "3:9999"] {
        let rows = events(&e, cursor);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["type"], "resync");
        assert_eq!(rows[1]["text"], "retained");
        assert!(events(&e, rows[1]["cursor"].as_str().unwrap()).is_empty());
    }
}

#[test]
fn notify_persists_reported_cursor_and_uses_it_after_restart() {
    use std::os::unix::fs::PermissionsExt;
    let dir = disk_tempdir();
    let fake = dir.path().join("forge");
    fs::write(
        &fake,
        r#"#!/bin/sh
case "$1" in
snapshot) echo '{"events_offset":"7:999"}' ;;
events) printf '%s\n' "$3" >> "$FORGE_PLUGIN_STATE/starts"
        echo '{"type":"note","text":"λ","cursor":"8:42"}' ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    for _ in 0..2 {
        let out = std::process::Command::new("sh")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/plugins/notify/notify.sh"
            ))
            .env("FORGE_BIN", &fake)
            .env("FORGE_PLUGIN_STATE", dir.path())
            .env("FORGE_PLUGIN_DIR", dir.path())
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    assert_eq!(
        fs::read_to_string(dir.path().join("cursor")).unwrap(),
        "8:42\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("starts")).unwrap(),
        "7:999\n8:42\n"
    );
}

fn follow_lines(e: &Env, want: usize) -> Vec<String> {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let mut child = e
        .cmd("ok.sh")
        .env("FORGE_PLUGIN_NAME", "replay")
        .args(["events", "--follow"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = Vec::new();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    while lines.len() < want {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        lines.push(line);
    }
    // Killed while blocked waiting for more, as a crash would.
    child.kill().unwrap();
    child.wait().unwrap();
    lines
}

#[test]
fn a_followed_subscription_resumes_after_a_kill_without_replaying_delivered_events() {
    let e = Env::new();
    fs::create_dir_all(&e.home).unwrap();
    let path = e.home.join("events.jsonl");
    let mut log = String::new();
    for i in 0..100 {
        log.push_str(&format!("{{\"type\":\"note\",\"text\":\"{i}\"}}\n"));
    }
    fs::write(&path, &log).unwrap();
    assert_eq!(follow_lines(&e, 100).len(), 100);
    let mut more = fs::OpenOptions::new().append(true).open(&path).unwrap();
    std::io::Write::write_all(&mut more, b"{\"type\":\"note\",\"text\":\"new\"}\n").unwrap();
    let again = follow_lines(&e, 1);
    assert_eq!(again.len(), 1);
    assert!(again[0].contains("\"new\""), "{again:?}");
}

/// Runs a reference plugin once against a stub `forge` whose `snapshot`
/// and `events` do what the given shell bodies say, and returns whether
/// it exited successfully. Output goes nowhere: github-issues' intake
/// loop leaves a `sleep` behind that would otherwise hold a pipe open.
fn run_plugin_with_stub(script: &str, dir: &std::path::Path, snapshot: &str, events: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};
    let fake = dir.join("forge");
    fs::write(
        &fake,
        format!("#!/bin/sh\ncase \"$1\" in\nsnapshot) {snapshot} ;;\nevents) {events} ;;\nesac\n"),
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        dir.join("config"),
        "GH_REPO=o/r\nTARGET_REPO=/nonexistent\nPOLL_SECONDS=30\n",
    )
    .unwrap();
    Command::new("sh")
        .arg(format!("{}/plugins/{script}", env!("CARGO_MANIFEST_DIR")))
        .env("FORGE_BIN", &fake)
        .env("FORGE_PLUGIN_STATE", dir)
        .env("FORGE_PLUGIN_DIR", dir)
        .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// REVIEW-4 E2-16: a failed first snapshot, an empty cursor file, or a
/// failed `events` must end the plugin non-zero so restart = "on-failure"
/// restarts it, not with the trailing loop's 0.
#[test]
fn reference_plugins_exit_non_zero_on_an_empty_offset_or_a_failed_events() {
    for script in ["notify/notify.sh", "github-issues/github-issues.sh"] {
        let ok_events = "echo '{\"type\":\"note\",\"cursor\":\"8:42\"}'";

        // The first snapshot fails: no offset to start from.
        let dir = disk_tempdir();
        assert!(
            !run_plugin_with_stub(script, dir.path(), "exit 1", ok_events),
            "{script}: failed snapshot"
        );
        assert!(!dir.path().join("cursor").exists(), "{script}");

        // A cursor file left empty by a kill mid-write: fail, and drop it
        // so the restart takes a fresh snapshot and recovers.
        let dir = disk_tempdir();
        fs::write(dir.path().join("cursor"), "").unwrap();
        let snapshot = "echo '{\"events_offset\":\"7:999\"}'";
        assert!(
            !run_plugin_with_stub(script, dir.path(), snapshot, ok_events),
            "{script}: empty cursor"
        );
        assert!(!dir.path().join("cursor").exists(), "{script}");
        assert!(
            run_plugin_with_stub(script, dir.path(), snapshot, ok_events),
            "{script}: restart after an empty cursor"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("cursor")).unwrap(),
            "8:42\n",
            "{script}"
        );
        assert!(!dir.path().join("cursor.tmp").exists(), "{script}");

        // The events process itself fails after delivering a line: the
        // cursor keeps what was delivered and the plugin exits non-zero.
        let dir = disk_tempdir();
        let failing = "echo '{\"type\":\"note\",\"cursor\":\"8:42\"}'; exit 3";
        assert!(
            !run_plugin_with_stub(script, dir.path(), snapshot, failing),
            "{script}: failed events"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("cursor")).unwrap(),
            "8:42\n",
            "{script}"
        );
    }
}
