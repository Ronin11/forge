//! The web client's messages-page rendering (`src/messages.js`): pure, no
//! DOM, so this runs it under `node` exactly the way `tests/doctor_render.rs`
//! runs `doctor.js` — a fixture of three messages and one decision
//! (`tests/fixtures/messages.json`, web UI task 10, "messages") renders
//! them in order, alongside the questions addressed to contacts and the
//! jobs a message triggered.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn a_fixture_of_three_messages_and_one_decision_renders_them_in_order() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/messages.test.js");
    let out = match Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping: node is not installed, so messages.js is not exercised");
            return;
        }
        Err(e) => panic!("running node: {e}"),
    };
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
