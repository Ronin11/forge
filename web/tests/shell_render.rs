//! The shared shell's rendering (`src/shell.js`): pure, no DOM, so this
//! runs it under `node` exactly the way `tests/time.rs` runs `time.js` —
//! a fixture snapshot (a worker, some tasks, and a `forge doctor --json`
//! read) renders the full nav and a populated header strip.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn a_fixture_snapshot_renders_the_full_nav_and_a_populated_header_strip() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/shell.test.js");
    let out = match Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping: node is not installed, so shell.js is not exercised");
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
