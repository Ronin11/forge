//! The web client's workflow rendering (`src/workflows.js`): pure, no DOM,
//! so this runs it under `node` exactly the way `tests/time.rs` runs
//! `time.js` — a fixture with two workflows renders two rows, and a lint
//! problem renders at its line.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn a_fixture_with_two_workflows_renders_two_rows_and_a_lint_problem_renders_at_its_line() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/workflows.test.js");
    let out = match Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping: node is not installed, so workflows.js is not exercised");
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
