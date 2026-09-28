//! The web client's Chat page rendering (`src/chat.js`): pure, no DOM, so
//! this runs it under `node` exactly as drafts_render.rs runs `drafts.js` —
//! a stream of events folds into a turn in flight, a stored session
//! renders with its tool calls, and a proposed action draws its confirm
//! and reject buttons while a decided one draws none.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn the_chat_page_folds_a_stream_into_a_turn_and_renders_proposals_with_their_buttons() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/chat.test.js");
    let out = match Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("skipping: node is not installed, so chat.js is not exercised");
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
