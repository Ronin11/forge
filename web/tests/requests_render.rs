//! The web client's inbox rendering (`src/requests.js`): pure, no DOM, so
//! this runs it under `node` exactly the way `tests/time.rs` runs
//! `time.js` — a fixture with two questions renders two inline answer
//! boxes, every row gets a withdraw control, and an unverified task
//! renders a land control.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn a_fixture_with_two_questions_renders_two_answer_boxes() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/requests.test.js");
    let out = match Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping: node is not installed, so requests.js is not exercised");
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
