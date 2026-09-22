//! The web client's initiative-page rendering (`src/initiative.js`): pure,
//! no DOM, so this runs it under `node` exactly the way
//! `tests/task_render.rs` runs `task.js` — a fixture built from
//! `tests/fixtures/initiative.json` (the shape `forge initiative report
//! --json` emits, `src/view.rs`'s `InitiativeDoc`) renders the
//! cost-vs-budget bar, the refused rules and a held reason, task 532's
//! requirement for the full `/initiatives/<id>` page.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn a_held_initiative_fixture_renders_the_bar_refused_rules_and_held_reason() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/initiative.test.js");
    let out = match Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping: node is not installed, so initiative.js is not exercised");
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
