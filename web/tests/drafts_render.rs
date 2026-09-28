//! The web client's draft editor rendering (`src/drafts.js`): pure, no DOM,
//! so this runs it under `node` exactly as workflows_render.rs runs
//! `workflows.js` — a two-step draft with a placeholder edits as data and
//! renders as a list plus an SVG of its edges.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn the_draft_editor_edits_a_step_list_and_renders_its_edges_and_placeholders() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/drafts.test.js");
    let out = match Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping: node is not installed, so drafts.js is not exercised");
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
