//! The portal's local-time script (`portal/src/local-time.js`) under a
//! fixed zone, compared to a checked-in snapshot. The script runs under
//! `node` with `TZ` pinned, so the viewer's own zone is the same on every
//! machine; explicit IANA names cover the rest. A machine without `node`
//! skips the test with a line on stderr — the rendered page itself, and
//! its UTC fallback text, is covered in `server.rs` either way.
//!
//! A snapshot is never written automatically. To update it deliberately
//! after a change to the script, re-run with `UPDATE_SNAPSHOTS=1`, then
//! read the diff in `git diff portal/tests/snapshots/` before committing.

use std::path::PathBuf;
use std::process::Command;

/// Loads the script as a module, then prints one line per case: a moment
/// formatted in each fixed zone and in the viewer's own (`TZ`), and what
/// `localize` does to a page's `<time>` elements — the prefix kept, a
/// broken `data-ts` left on its server text.
const HARNESS: &str = r#"
const script = require(process.argv[1]);
const moments = [0, 1699999999, 1720000000, 1726531200];
for (const zone of ["UTC", "America/New_York", "Asia/Kolkata", "Pacific/Auckland", undefined]) {
  for (const ts of moments) {
    console.log(`${zone || "viewer"} ${ts} ${script.format(ts, zone)}`);
  }
}
const attrs = [
  { "data-ts": "1699999999", "data-prefix": "Shipped " },
  { "data-ts": "1700000000" },
  { "data-ts": "soon", "data-prefix": "Asked " },
];
const els = attrs.map((a) => ({
  textContent: "server text",
  getAttribute: (k) => (k in a ? a[k] : null),
}));
script.localize({ querySelectorAll: () => els }, "America/New_York");
for (const e of els) console.log(`localize ${e.textContent}`);
"#;

#[test]
fn moments_render_in_a_fixed_zone_and_the_viewers_own() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/local-time.js");
    let out = match Command::new("node")
        .args(["-e", HARNESS])
        .arg(&script)
        .env("TZ", "Asia/Tokyo")
        .output()
    {
        Ok(o) if o.status.success() => o,
        Ok(o) => panic!("node failed: {}", String::from_utf8_lossy(&o.stderr)),
        Err(e) => {
            eprintln!("skipping: node is not runnable here ({e})");
            return;
        }
    };
    let got = String::from_utf8(out.stdout).unwrap();
    let snap = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/local_time.txt");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&snap, &got).unwrap();
    }
    let want = std::fs::read_to_string(&snap).unwrap_or_default();
    assert_eq!(
        got, want,
        "the local-time snapshot changed; re-run with UPDATE_SNAPSHOTS=1 if that is intended"
    );
}
