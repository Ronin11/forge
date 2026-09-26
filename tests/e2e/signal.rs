use crate::support::*;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

struct SignalFixture {
    _tmp: tempfile::TempDir,
    plugin_dir: PathBuf,
    state_dir: PathBuf,
    message: PathBuf,
    signal_cli: PathBuf,
}

/// The plugin's dirs, its config for contact alice on project demo, the
/// one scripted inbound message, and a fake `signal-cli` that records
/// what it is asked to send in `$FORGE_PLUGIN_STATE/sent`.
fn signal_fixture(repo: &str) -> SignalFixture {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("plugin");
    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(
        plugin_dir.join("config"),
        format!(
            "SIGNAL_ACCOUNT=+15555550100\n\
             SIGNAL_TO=+15555550199\n\
             CONTACTS=alice:+15555550111\n\
             PROJECTS=alice:demo\n\
             POLL_SECONDS=1\n\
             TARGET_REPO={repo}\n\
             WORKFLOW=direct\n"
        ),
    )
    .unwrap();
    let message = tmp.path().join("message.json");
    std::fs::write(
        &message,
        "{\"envelope\":{\"source\":\"+15555550111\",\"sourceNumber\":\"+15555550111\",\
         \"dataMessage\":{\"message\":\"Please change the price.\"}}}\n",
    )
    .unwrap();
    let signal_cli = tmp.path().join("signal-cli-fake.sh");
    std::fs::write(
        &signal_cli,
        r#"#!/bin/sh
set -u
shift
shift
cmd=$1
shift
case "$cmd" in
    send)
        msg=""
        dest=""
        while [ $# -gt 0 ]; do
            case "$1" in
                -m) msg=$2; shift 2 ;;
                -g) dest=$2; shift 2 ;;
                *) dest=$1; shift ;;
            esac
        done
        printf 'SEND %s %s\n' "$dest" "$msg" >>"$FORGE_PLUGIN_STATE/sent"
        ;;
    receive)
        flag="$FORGE_PLUGIN_STATE/received"
        if [ ! -f "$flag" ]; then
            touch "$flag"
            cat "$FAKE_MESSAGE_FILE"
        fi
        ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&signal_cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    SignalFixture {
        _tmp: tmp,
        plugin_dir,
        state_dir,
        message,
        signal_cli,
    }
}

/// Runs `signal.sh` directly (as the sibling test in plugins.rs does)
/// with the given fake concierge agent.
fn spawn_signal(e: &Env, f: &SignalFixture, fake: &str) -> std::process::Child {
    let signal_sh =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/signal/signal.sh");
    let claude_fake = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fakes")
        .join(fake);
    let mut cmd = Command::new("sh");
    cmd.arg(&signal_sh)
        .env("FORGE_BIN", env!("CARGO_BIN_EXE_forge"))
        .env("FORGE_HOME", &e.home)
        .env("FORGE_CLAUDE_BIN", &claude_fake)
        .env("FORGE_SUPERVISOR", "0")
        .env("FORGE_PLUGIN_DIR", &f.plugin_dir)
        .env("FORGE_PLUGIN_STATE", &f.state_dir)
        .env("SIGNAL_CLI", &f.signal_cli)
        .env("FAKE_MESSAGE_FILE", &f.message)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if e.sandbox_disabled() {
        cmd.env("FORGE_SANDBOX", "0");
    }

    cmd.spawn().unwrap()
}

/// The concierge's "unclear" decision blocks a task with a question
/// addressed to the contact; the plugin's send of that question must be
/// recorded against that task, so `delivered_at` is set and doctor has
/// no undelivered-question row for it.
#[test]
fn the_signal_plugin_records_a_concierge_question_against_its_task() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "demo",
                "--purpose",
                "Demo runs a small repair shop over text messages.",
                "--repo",
                repo,
            ],
        )
        .status
        .success()
    );

    let f = signal_fixture(repo);
    let mut child = spawn_signal(&e, &f, "concierge-unclear.sh");

    let sent = f.state_dir.join("sent");
    assert!(
        wait_until(
            || std::fs::read_to_string(&sent)
                .unwrap_or_default()
                .contains("price change applied"),
            Duration::from_secs(30),
        ),
        "expected the question to be sent; sent so far: {:?}",
        std::fs::read_to_string(&sent)
    );
    // The recording runs after the send, as its own call.
    let delivered = wait_until(
        || {
            e.requests_json()
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["to"] == "alice" && r["delivered_at"].as_i64().is_some())
        },
        Duration::from_secs(30),
    );
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let _ = child.wait();

    let requests = e.requests_json();
    assert!(delivered, "delivered_at never set: {requests:?}");
    let r = requests
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["to"] == "alice")
        .unwrap();
    let id = r["task"].as_i64().or_else(|| r["id"].as_i64()).unwrap();
    let doctor = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&doctor.stdout);
    assert!(
        !out.contains(&format!("task {id} to alice")),
        "doctor still flags the question: {out}"
    );
}
