//! The web client's deploys-page rendering (`src/deploys.js`): pure, no
//! DOM, so this runs it under `node` exactly the way
//! `tests/initiative_render.rs` runs `initiative.js` — a fixture built
//! from `tests/fixtures/deploys.json` (one target, two deploys) renders
//! both deploys' check/smoke/look verdicts, the rollback reason, and the
//! screenshot `<img>` tag, task 534's requirement for the `/deploys` page.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn a_target_with_two_deploys_renders_both_verdicts_and_the_screenshot_tag() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/deploys.test.js");
    let out = match Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping: node is not installed, so deploys.js is not exercised");
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
