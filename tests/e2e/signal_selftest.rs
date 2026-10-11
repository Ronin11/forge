//! `forge plugin test signal` (docs/PLUGINS.md, "Self-test") against a
//! fake `signal-cli` that, like a real one, takes `-o json` before the
//! subcommand and delivers a message sent to the account's own number back
//! to its next `receive`.
use crate::support::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const ACCOUNT: &str = "+15555550100";

const FAKE: &str = r#"#!/bin/sh
acct=$2
shift 2
[ "$1" = -o ] && shift 2
cmd=$1
shift
inbox="$FORGE_PLUGIN_STATE/fake-inbox"
case "$cmd" in
    send)
        msg=""
        dest=""
        while [ $# -gt 0 ]; do
            case "$1" in
                -m) msg=$2; shift 2 ;;
                *) dest=$1; shift ;;
            esac
        done
        printf '%s|%s\n' "$dest" "$msg" >>"$FORGE_PLUGIN_STATE/fake-sent"
        if [ "$dest" = "$acct" ]; then
            printf '{"envelope":{"source":"%s","sourceNumber":"%s","syncMessage":{"sentMessage":{"message":"%s"}}}}\n' \
                "$acct" "$acct" "$msg" >>"$inbox"
        fi
        ;;
    receive)
        if [ -f "$FORGE_PLUGIN_STATE/fake-broken" ]; then
            echo "signal-cli: error: unrecognized arguments: receive is broken here" >&2
            exit 2
        fi
        if mv "$inbox" "$inbox.taken" 2>/dev/null; then
            cat "$inbox.taken"
            rm -f "$inbox.taken"
        fi
        ;;
esac
"#;

struct Fixture {
    state: PathBuf,
    path: String,
}

fn fixture(e: &Env) -> Fixture {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/signal");
    let out = e.forge("ok.sh", &["plugin", "install", src.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::write(
        e.home.join("plugins/signal/config"),
        format!(
            "SIGNAL_ACCOUNT={ACCOUNT}\nSIGNAL_TO=+15555550199\nPOLL_SECONDS=1\nSELFTEST_WAIT=20\n"
        ),
    )
    .unwrap();
    let stub_dir = e._dir.path().join("stub-bin");
    std::fs::create_dir_all(&stub_dir).unwrap();
    let fake = stub_dir.join("signal-cli");
    std::fs::write(&fake, FAKE).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let state = e.home.join("plugins-state/signal");
    std::fs::create_dir_all(&state).unwrap();
    let path = format!("{}:{}", stub_dir.display(), std::env::var("PATH").unwrap());
    Fixture { state, path }
}

/// The installed plugin, run the way the supervisor runs it, its stderr
/// into the plugin's log.
fn spawn(e: &Env, f: &Fixture) -> Worker {
    let log_dir = e.home.join("logs/plugins");
    std::fs::create_dir_all(&log_dir).unwrap();
    let log = std::fs::File::create(log_dir.join("signal.log")).unwrap();
    let mut cmd = Command::new("sh");
    cmd.arg("./signal.sh")
        .current_dir(e.home.join("plugins/signal"))
        .env("PATH", &f.path)
        .env("FORGE_BIN", env!("CARGO_BIN_EXE_forge"))
        .env("FORGE_HOME", &e.home)
        .env("FORGE_PLUGIN_NAME", "signal")
        .env("FORGE_PLUGIN_DIR", e.home.join("plugins/signal"))
        .env("FORGE_PLUGIN_STATE", &f.state)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    Worker::spawn(&mut cmd)
}

fn plugin_test(e: &Env, f: &Fixture) -> (bool, String) {
    let out = e
        .cmd("ok.sh")
        .env("PATH", &f.path)
        .args(["plugin", "test", "signal"])
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn the_signal_self_test_sends_itself_a_message_and_sees_the_receive_loop_get_it() {
    let e = Env::new();
    let f = fixture(&e);

    let (ok, said) = plugin_test(&e, &f);
    assert!(!ok, "no receive loop is running yet: {said}");
    assert!(said.contains("no heartbeat"), "{said}");

    let mut plugin = spawn(&e, &f);
    assert!(
        wait_until(
            || f.state.join("heartbeat").exists(),
            Duration::from_secs(20)
        ),
        "the receive loop never beat"
    );
    let (ok, said) = plugin_test(&e, &f);
    assert!(ok, "{said}");
    assert!(said.contains("ok, sent forge-selftest-"), "{said}");

    // Doctor runs the same self-test for the enabled plugin.
    assert!(
        e.forge("ok.sh", &["plugin", "enable", "signal"])
            .status
            .success()
    );
    let out = e
        .cmd("ok.sh")
        .env("PATH", &f.path)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    plugin.stop();
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "plugins.signal.test")
        .unwrap_or_else(|| panic!("no self-test row: {rows}"));
    assert_eq!(row["status"], "ok", "{row}");

    // The self-test's messages went to the account itself, and were never
    // routed as work or a reply.
    let sent = std::fs::read_to_string(f.state.join("fake-sent")).unwrap();
    assert!(
        sent.lines()
            .all(|l| l.starts_with(&format!("{ACCOUNT}|forge-selftest-"))),
        "{sent}"
    );
    assert_eq!(sent.lines().count(), 2, "{sent}");
}

#[test]
fn a_failing_receive_says_why_in_the_log_and_fails_the_self_test() {
    let e = Env::new();
    let f = fixture(&e);
    std::fs::write(f.state.join("fake-broken"), "").unwrap();
    let mut plugin = spawn(&e, &f);
    assert!(
        wait_until(
            || std::fs::read_to_string(f.state.join("heartbeat"))
                .is_ok_and(|t| t.contains("exit 2")),
            Duration::from_secs(20)
        ),
        "no failed beat: {:?}",
        std::fs::read_to_string(f.state.join("heartbeat"))
    );
    let (ok, said) = plugin_test(&e, &f);
    plugin.stop();
    assert!(!ok, "{said}");
    assert!(said.contains("the last receive failed (exit 2)"), "{said}");
    let log = std::fs::read_to_string(e.home.join("logs/plugins/signal.log")).unwrap();
    assert!(
        log.contains("unrecognized arguments: receive is broken here"),
        "signal-cli's stderr belongs in the plugin log: {log}"
    );
    assert!(log.contains("signal-cli receive exited 2"), "{log}");
}
