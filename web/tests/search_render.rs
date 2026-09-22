//! The /tasks list's filters (`src/search.js`): pure, no DOM, so this
//! runs it under `node` exactly the way `tests/shell_render.rs` runs
//! `shell.js` — the filters `forge log --json` itself takes round-trip
//! through a URL query string.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn the_tasks_list_filters_round_trip_through_a_url_query_string() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/search.test.js");
    let out = match Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping: node is not installed, so search.js is not exercised");
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
