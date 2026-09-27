//! The presence plugin end to end: a `command` source's transitions move
//! the state it records and apply the configured `CPUWeight` through
//! `systemctl --user set-property --runtime`, faked here rather than
//! touching a real systemd user session.

use crate::support::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn write_exec(path: &Path, script: &str) {
    std::fs::write(path, script).unwrap();
    let mut perm = std::fs::metadata(path).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm).unwrap();
}

/// A fake `systemctl`: records every call (one line per invocation) to
/// `$HOME/systemctl-calls.log` rather than touching a real user session.
const FAKE_SYSTEMCTL: &str = "#!/bin/sh\necho \"systemctl $*\" >> \"$HOME/systemctl-calls.log\"\n";

/// A fake `command` source: one transition, a pause, then another, so the
/// plugin's optimistic initial `active` and both lines it prints are each
/// observable as their own state-file write before the process sleeps
/// (never exiting, so the manifest's `restart = "on-failure"` never fires
/// mid-test and confuses which run produced which log lines).
const FAKE_SOURCE: &str = "#!/bin/sh\necho idle\nsleep 0.3\necho active\nsleep 1000\n";

fn install_and_enable(e: &Env, source_dir: &Path) {
    assert!(
        e.forge(
            "ok.sh",
            &["plugin", "install", source_dir.to_str().unwrap()]
        )
        .status
        .success()
    );
    assert!(
        e.forge("ok.sh", &["plugin", "enable", "presence"])
            .status
            .success()
    );
}

fn state_path(home: &Path) -> PathBuf {
    home.join("plugins-state/presence/state")
}

fn read_state(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn state_is(path: &Path, want: &str) -> bool {
    read_state(path)
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("state="))
                .map(String::from)
        })
        .as_deref()
        == Some(want)
}

#[test]
fn a_fake_source_commands_transitions_move_the_recorded_state_and_call_a_fake_systemctl() {
    let e = Env::new();
    let plugin_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/presence");
    install_and_enable(&e, &plugin_src);

    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("fake-source.sh");
    write_exec(&source, FAKE_SOURCE);
    std::fs::write(
        e.home.join("plugins/presence/config"),
        format!(
            "SOURCE=command\nCOMMAND={}\nUNIT=forge-worker\nACTIVE_WEIGHT=40\nIDLE_WEIGHT=100\n",
            source.display()
        ),
    )
    .unwrap();

    let bin = scratch.path().join("fake-bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exec(&bin.join("systemctl"), FAKE_SYSTEMCTL);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let fake_home = tempfile::tempdir().unwrap();
    let calls_log = fake_home.path().join("systemctl-calls.log");

    let mut worker = Worker::spawn(
        e.cmd("ok.sh")
            .env("HOME", fake_home.path())
            .env("PATH", path)
            .args(["work", "--poll", "1"]),
    );

    let state = state_path(&e.home);
    assert!(
        wait_until(|| state_is(&state, "idle"), Duration::from_secs(10)),
        "expected the fake source's first line to move the state to idle: {:?}",
        read_state(&state)
    );
    assert!(
        wait_until(|| state_is(&state, "active"), Duration::from_secs(10)),
        "expected the fake source's second line to move the state back to active: {:?}",
        read_state(&state)
    );

    assert!(
        wait_until(
            || std::fs::read_to_string(&calls_log)
                .is_ok_and(|t| t.matches("CPUWeight=").count() >= 3),
            Duration::from_secs(10)
        ),
        "expected at least 3 systemctl calls (the initial active, then idle, then active): {:?}",
        std::fs::read_to_string(&calls_log)
    );
    let calls = std::fs::read_to_string(&calls_log).unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    assert!(
        lines[0].contains("set-property --runtime forge-worker CPUWeight=40"),
        "{calls}"
    );
    assert!(
        lines[1].contains("set-property --runtime forge-worker CPUWeight=100"),
        "{calls}"
    );
    assert!(
        lines[2].contains("set-property --runtime forge-worker CPUWeight=40"),
        "{calls}"
    );

    worker.stop();
}

#[test]
fn doctor_reports_the_presence_row_once_a_state_is_written() {
    let e = Env::new();
    let plugin_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/presence");
    install_and_enable(&e, &plugin_src);

    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("fake-source.sh");
    write_exec(&source, "#!/bin/sh\nsleep 1000\n");
    std::fs::write(
        e.home.join("plugins/presence/config"),
        format!(
            "SOURCE=command\nCOMMAND={}\nUNIT=forge-worker\n",
            source.display()
        ),
    )
    .unwrap();

    let bin = scratch.path().join("fake-bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exec(&bin.join("systemctl"), FAKE_SYSTEMCTL);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fake_home = tempfile::tempdir().unwrap();

    let mut worker = Worker::spawn(
        e.cmd("ok.sh")
            .env("HOME", fake_home.path())
            .env("PATH", path)
            .args(["work", "--poll", "1"]),
    );

    let state = state_path(&e.home);
    assert!(
        wait_until(|| state_is(&state, "active"), Duration::from_secs(10)),
        "expected the plugin's optimistic initial state: {:?}",
        read_state(&state)
    );

    assert!(
        wait_until(
            || {
                let o = e.forge("ok.sh", &["doctor", "--json"]);
                let Ok(checks) = serde_json::from_slice::<Vec<serde_json::Value>>(&o.stdout) else {
                    return false;
                };
                checks.iter().any(|c| {
                    c["name"] == "presence"
                        && c["status"] == "ok"
                        && c["detail"].as_str().is_some_and(|d| {
                            d.starts_with("active since ") && d.ends_with(", weight 40")
                        })
                })
            },
            Duration::from_secs(10)
        ),
        "expected a presence row naming the active state and its weight"
    );

    worker.stop();
}
