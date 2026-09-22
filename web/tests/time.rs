//! The web client's time helper (`src/time.js`) renders Unix seconds in the
//! viewer's zone. `tests/time.test.js` checks a known instant; this runs it
//! under `node` once per zone with TZ set, since the zone is process-wide.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn the_time_helper_renders_a_known_instant_in_each_fixed_zone() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/time.test.js");
    for tz in [
        "UTC",
        "America/New_York",
        "Asia/Kolkata",
        "Pacific/Auckland",
    ] {
        let out = match Command::new("node").arg(script).env("TZ", tz).output() {
            Ok(out) => out,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                eprintln!("skipping: node is not installed, so time.js is not exercised");
                return;
            }
            Err(e) => panic!("running node: {e}"),
        };
        assert!(
            out.status.success(),
            "TZ={tz}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn app_js_formats_no_time_itself() {
    let app = include_str!("../src/app.js");
    for banned in [
        "toLocale",
        "toISOString",
        "toUTCString",
        "getTimezoneOffset",
    ] {
        assert!(
            !app.contains(banned),
            "app.js formats a time with {banned}; go through ForgeTime"
        );
    }
    assert!(
        !app.contains("new Date("),
        "app.js builds a Date itself; go through ForgeTime"
    );
}

#[test]
fn shell_js_formats_no_time_itself() {
    let shell = include_str!("../src/shell.js");
    for banned in [
        "toLocale",
        "toISOString",
        "toUTCString",
        "getTimezoneOffset",
        "new Date(",
    ] {
        assert!(
            !shell.contains(banned),
            "shell.js formats a time with {banned}; every time it shows must go through the \
             `fmtTime` the caller passes in, never a formatter of its own"
        );
    }
}

#[test]
fn task_js_formats_no_time_itself() {
    let task = include_str!("../src/task.js");
    for banned in [
        "toLocale",
        "toISOString",
        "toUTCString",
        "getTimezoneOffset",
        "new Date(",
    ] {
        assert!(
            !task.contains(banned),
            "task.js formats a time with {banned}; every time it shows must go through the \
             `fmtTime` the caller passes in, never a formatter of its own"
        );
    }
}

#[test]
fn initiative_js_formats_no_time_itself() {
    let initiative = include_str!("../src/initiative.js");
    for banned in [
        "toLocale",
        "toISOString",
        "toUTCString",
        "getTimezoneOffset",
        "new Date(",
    ] {
        assert!(
            !initiative.contains(banned),
            "initiative.js formats a time with {banned}; every time it shows must go through \
             the `fmtSpan` the caller passes in, never a formatter of its own"
        );
    }
}

#[test]
fn stats_js_formats_no_time_itself() {
    let stats = include_str!("../src/stats.js");
    for banned in [
        "toLocale",
        "toISOString",
        "toUTCString",
        "getTimezoneOffset",
        "new Date(",
    ] {
        assert!(
            !stats.contains(banned),
            "stats.js formats a time with {banned}; StatsDoc.daily's `date` is a plain UTC \
             calendar string already, never a formatter of its own"
        );
    }
}
