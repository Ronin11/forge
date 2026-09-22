//! The web client's task-page rendering (`src/task.js`): pure, no DOM, so
//! this runs it under `node` exactly the way `tests/requests_render.rs`
//! runs `requests.js` — a fixture built from `tests/fixtures/trace.json`
//! (the shape `forge trace --json` emits, `src/view.rs`'s `TraceDoc`)
//! renders every step and attempt it carries, task 531's requirement for
//! the full `/tasks/<id>` page.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn a_trace_fixture_renders_every_step_and_attempt() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/task.test.js");
    let out = match Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping: node is not installed, so task.js is not exercised");
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
