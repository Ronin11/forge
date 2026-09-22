//! The web client's module-graph rendering (`src/graph.js`): pure, no
//! DOM, so this runs it under `node` exactly the way `tests/time.rs` runs
//! `time.js` — a three-node, two-edge fixture renders three nodes and
//! two edges, and the overlay shows up as a cost colour and a demotion
//! badge.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn a_three_node_two_edge_fixture_renders_three_nodes_and_two_edges() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/graph.test.js");
    let out = match Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping: node is not installed, so graph.js is not exercised");
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
