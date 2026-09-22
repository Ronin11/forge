//! The web client's activity-page rendering (`src/activity.js`): pure,
//! no DOM, so this runs it under `node` exactly the way
//! `tests/doctor_render.rs` runs `doctor.js` — a fixture event stream
//! (`tests/fixtures/activity.json`) renders the right kinds, newest
//! first, and each of project/kind/task narrows it (web UI task 8,
//! "activity").

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn a_fixture_event_stream_renders_the_right_kinds_and_the_filters_narrow_it() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/activity.test.js");
    let out = match Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping: node is not installed, so activity.js is not exercised");
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
