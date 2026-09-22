//! The web client's doctor-page rendering (`src/doctor.js`): pure, no
//! DOM, so this runs it under `node` exactly the way
//! `tests/deploys_render.rs` runs `deploys.js` — a fixture built from
//! `tests/fixtures/doctor.json` (a doctor document with one WARN check,
//! `worktrees`) renders every check as a row and, specifically, the WARN
//! check's own fix line, task 7's requirement for the `/doctor` page.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn a_doctor_document_with_one_warn_check_renders_its_fix_line() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/doctor.test.js");
    let out = match Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping: node is not installed, so doctor.js is not exercised");
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
