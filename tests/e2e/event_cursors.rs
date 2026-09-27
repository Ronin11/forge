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
    let dir = tempfile::tempdir().unwrap();
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
