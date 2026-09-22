//! The web client's stats rendering (`src/stats.js`): pure, no DOM, so
//! this runs it under `node` exactly the way `tests/workflows_render.rs`
//! runs `workflows.js` — a fixture `StatsDoc` renders every tab and the
//! chart's 30 points.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn a_fixture_stats_document_renders_every_tab_and_the_charts_30_points() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/stats.test.js");
    let out = match Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping: node is not installed, so stats.js is not exercised");
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
